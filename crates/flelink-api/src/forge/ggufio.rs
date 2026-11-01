//! Reading and writing GGUF files for the Forge. Port of `fle_studio_forge/ggufio.py`.
//!
//! `crate::gguf` only needs sizes and a few metadata values. The Forge copies a file: the metadata is carried over byte for
//! byte, and tensors are streamed from the sources into the new file in chunks, so a model of any size needs a few MiB of
//! memory. Shapes are kept in GGUF order (`ne0` first). For the tensors of a mixture-of-experts model the expert axis is the
//! last entry of the shape and the slowest axis in the data: one expert is one contiguous slice of `nbytes / n_expert`.
use sha2::Digest;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const MAGIC: &[u8; 4] = b"GGUF";
pub const DEFAULT_ALIGNMENT: u64 = 32;
pub const CHUNK: usize = 8 * 1024 * 1024;

pub const T_U8: u32 = 0;
pub const T_I8: u32 = 1;
pub const T_U16: u32 = 2;
pub const T_I16: u32 = 3;
pub const T_U32: u32 = 4;
pub const T_I32: u32 = 5;
pub const T_F32: u32 = 6;
pub const T_BOOL: u32 = 7;
pub const T_STRING: u32 = 8;
pub const T_ARRAY: u32 = 9;
pub const T_U64: u32 = 10;
pub const T_I64: u32 = 11;
pub const T_F64: u32 = 12;

#[derive(Debug, Clone)]
pub struct GgufError(pub String);

impl std::fmt::Display for GgufError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type R<T> = Result<T, GgufError>;

fn gerr<T>(m: impl Into<String>) -> R<T> {
    Err(GgufError(m.into()))
}

/// ggml tensor types: id -> (name, elements per block, bytes per block). A type this table does not know is still copied
/// correctly (sizes then come from the header offsets).
pub const TYPES: [(u32, &str, u64, u64); 32] = [
    (0, "F32", 1, 4),
    (1, "F16", 1, 2),
    (2, "Q4_0", 32, 18),
    (3, "Q4_1", 32, 20),
    (6, "Q5_0", 32, 22),
    (7, "Q5_1", 32, 24),
    (8, "Q8_0", 32, 34),
    (9, "Q8_1", 32, 36),
    (10, "Q2_K", 256, 84),
    (11, "Q3_K", 256, 110),
    (12, "Q4_K", 256, 144),
    (13, "Q5_K", 256, 176),
    (14, "Q6_K", 256, 210),
    (15, "Q8_K", 256, 292),
    (16, "IQ2_XXS", 256, 66),
    (17, "IQ2_XS", 256, 74),
    (18, "IQ3_XXS", 256, 98),
    (19, "IQ1_S", 256, 50),
    (20, "IQ4_NL", 32, 18),
    (21, "IQ3_S", 256, 110),
    (22, "IQ2_S", 256, 82),
    (23, "IQ4_XS", 256, 136),
    (24, "I8", 1, 1),
    (25, "I16", 1, 2),
    (26, "I32", 1, 4),
    (27, "I64", 1, 8),
    (28, "F64", 1, 8),
    (29, "IQ1_M", 256, 56),
    (30, "BF16", 1, 2),
    (34, "TQ1_0", 256, 54),
    (35, "TQ2_0", 256, 66),
    (39, "MXFP4", 32, 17),
];

fn type_info(id: u32) -> Option<(&'static str, u64, u64)> {
    TYPES
        .iter()
        .find(|(i, _, _, _)| *i == id)
        .map(|(_, n, b, s)| (*n, *b, *s))
}

pub fn type_name(id: u32) -> String {
    type_info(id).map_or_else(|| format!("TYPE_{id}"), |(n, _, _)| n.to_string())
}

pub fn type_id(name: &str) -> Option<u32> {
    let up = name.to_uppercase();
    TYPES
        .iter()
        .find(|(_, n, _, _)| *n == up)
        .map(|(i, _, _, _)| *i)
}

/// Types llama-quantize accepts as a target (lower case), sorted.
pub fn quantize_targets() -> Vec<String> {
    let mut v: Vec<String> = TYPES
        .iter()
        .filter(|(_, n, _, _)| {
            !["Q8_K", "Q8_1", "I8", "I16", "I32", "I64", "F64", "MXFP4"].contains(n)
        })
        .map(|(_, n, _, _)| n.to_lowercase())
        .collect();
    v.sort();
    v
}

/// The named mixtures llama-quantize accepts as its target, with the tensor type each is built around.
pub const FTYPE_BASE: [(&str, &str); 11] = [
    ("q2_k_s", "q2_k"),
    ("q3_k_s", "q3_k"),
    ("q3_k_m", "q3_k"),
    ("q3_k_l", "q3_k"),
    ("q4_k_s", "q4_k"),
    ("q4_k_m", "q4_k"),
    ("q5_k_s", "q5_k"),
    ("q5_k_m", "q5_k"),
    ("iq3_xs", "iq3_xxs"),
    ("iq3_m", "iq3_s"),
    ("iq2_m", "iq2_s"),
];

pub fn ftype_base(t: &str) -> &str {
    FTYPE_BASE
        .iter()
        .find(|(k, _)| *k == t)
        .map_or(t, |(_, v)| v)
}

pub fn quantize_ftypes() -> Vec<String> {
    let mut v = quantize_targets();
    v.extend(FTYPE_BASE.iter().map(|(k, _)| k.to_string()));
    v.sort();
    v.dedup();
    v
}

/// Bytes of one row of `ne0` elements, or None for an unknown type or a row that is not a whole number of blocks.
pub fn row_bytes(t: u32, ne0: u64) -> Option<u64> {
    let (_, block, size) = type_info(t)?;
    (ne0 % block == 0).then(|| ne0 / block * size)
}

pub fn tensor_nbytes(t: u32, shape: &[u64]) -> Option<u64> {
    let row = row_bytes(t, *shape.first()?)?;
    Some(row * shape[1..].iter().product::<u64>())
}

// ------------------------------------------------------------------------------------------------ reading

/// One metadata entry. `raw` is the encoded value (without key and type) and is written back unchanged.
#[derive(Debug, Clone)]
pub struct Kv {
    pub key: String,
    pub vtype: u32,
    pub raw: Vec<u8>,
    /// scalars and strings; None for arrays
    pub value: Option<serde_json::Value>,
}

impl Kv {
    pub fn encoded(&self) -> Vec<u8> {
        let mut out = encode_string(&self.key);
        out.extend(self.vtype.to_le_bytes());
        out.extend(&self.raw);
        out
    }
}

#[derive(Debug, Clone)]
pub struct TensorInfo {
    pub name: String,
    pub shape: Vec<u64>,
    pub ttype: u32,
    /// from the start of the data section of its file
    pub offset: u64,
    pub nbytes: u64,
    /// index of the file this tensor lives in
    pub part: usize,
}

impl TensorInfo {
    pub fn type_name(&self) -> String {
        type_name(self.ttype)
    }
    pub fn n_elements(&self) -> u64 {
        self.shape.iter().product()
    }
}

#[derive(Debug, Clone)]
pub struct GgufFile {
    pub path: String,
    pub version: u32,
    pub kv: Vec<Kv>,
    pub tensors: Vec<TensorInfo>,
    pub data_start: u64,
    pub alignment: u64,
    pub file_size: u64,
}

struct Reader<'a> {
    f: &'a mut std::io::BufReader<File>,
}

impl Reader<'_> {
    fn bytes(&mut self, n: usize) -> R<Vec<u8>> {
        let mut b = vec![0u8; n];
        self.f
            .read_exact(&mut b)
            .map_err(|_| GgufError("the file ends inside its header".into()))?;
        Ok(b)
    }
    fn u32(&mut self) -> R<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> R<u64> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }
    fn string(&mut self) -> R<String> {
        let n = self.u64()?;
        if n > 1 << 30 {
            return gerr("implausible string length in header");
        }
        Ok(String::from_utf8_lossy(&self.bytes(n as usize)?).to_string())
    }
    /// The value of a metadata entry: decoded for scalars and strings, skipped (None) for arrays.
    fn value(&mut self, vtype: u32) -> R<Option<serde_json::Value>> {
        use serde_json::json;
        Ok(Some(match vtype {
            T_U8 => json!(self.bytes(1)?[0]),
            T_I8 => json!(self.bytes(1)?[0] as i8),
            T_U16 => json!(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap())),
            T_I16 => json!(i16::from_le_bytes(self.bytes(2)?.try_into().unwrap())),
            T_U32 => json!(self.u32()?),
            T_I32 => json!(self.u32()? as i32),
            T_F32 => json!(f32::from_le_bytes(self.bytes(4)?.try_into().unwrap())),
            T_BOOL => json!(self.bytes(1)?[0] != 0),
            T_STRING => json!(self.string()?),
            T_U64 => json!(self.u64()?),
            T_I64 => json!(self.u64()? as i64),
            T_F64 => json!(f64::from_le_bytes(self.bytes(8)?.try_into().unwrap())),
            T_ARRAY => {
                let et = self.u32()?;
                let n = self.u64()?;
                if n > 100_000_000 {
                    return gerr("implausible array length in header");
                }
                let fixed = match et {
                    T_U8 | T_I8 | T_BOOL => Some(1),
                    T_U16 | T_I16 => Some(2),
                    T_U32 | T_I32 | T_F32 => Some(4),
                    T_U64 | T_I64 | T_F64 => Some(8),
                    _ => None,
                };
                match fixed {
                    Some(w) => {
                        self.f
                            .seek_relative((w * n) as i64)
                            .map_err(|e| GgufError(e.to_string()))?;
                    }
                    None => {
                        for _ in 0..n {
                            self.value(et)?;
                        }
                    }
                }
                return Ok(None);
            }
            other => return gerr(format!("unknown metadata type {other}")),
        }))
    }
}

/// Parse the whole header of one file: raw metadata, tensor table, where the data starts.
pub fn read_gguf(path: &Path, part: usize) -> R<GgufFile> {
    let file = File::open(path).map_err(|e| GgufError(format!("{}: {e}", path.display())))?;
    let file_size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = std::io::BufReader::new(file);
    let mut r = Reader { f: &mut buf };
    if r.bytes(4)? != MAGIC {
        return gerr("not a GGUF file");
    }
    let version = r.u32()?;
    if version != 2 && version != 3 {
        return gerr(format!("GGUF version {version} is not supported"));
    }
    let (n_tensors, n_kv) = (r.u64()?, r.u64()?);
    if n_tensors > 2_000_000 || n_kv > 1_000_000 {
        return gerr("implausible header (not a GGUF file?)");
    }
    let mut spans: Vec<(String, u32, u64, u64, Option<serde_json::Value>)> = vec![];
    for _ in 0..n_kv {
        let key = r.string()?;
        let vtype = r.u32()?;
        let start =
            r.f.stream_position()
                .map_err(|e| GgufError(e.to_string()))?;
        let value = r.value(vtype)?;
        let end =
            r.f.stream_position()
                .map_err(|e| GgufError(e.to_string()))?;
        spans.push((key, vtype, start, end, value));
    }
    let mut raw: Vec<(String, Vec<u64>, u32, u64)> = vec![];
    for _ in 0..n_tensors {
        let name = r.string()?;
        let n_dims = r.u32()?;
        if n_dims > 8 {
            return gerr("implausible tensor rank in header");
        }
        let mut dims = vec![];
        for _ in 0..n_dims {
            dims.push(r.u64()?);
        }
        let t = r.u32()?;
        let off = r.u64()?;
        raw.push((name, dims, t, off));
    }
    let header_end = buf
        .stream_position()
        .map_err(|e| GgufError(e.to_string()))?;
    let mut kv = vec![];
    for (key, vtype, start, end, value) in spans {
        buf.seek(SeekFrom::Start(start))
            .map_err(|e| GgufError(e.to_string()))?;
        let mut b = vec![0u8; (end - start) as usize];
        buf.read_exact(&mut b)
            .map_err(|e| GgufError(e.to_string()))?;
        kv.push(Kv {
            key,
            vtype,
            raw: b,
            value,
        });
    }
    let alignment = kv
        .iter()
        .find(|k| k.key == "general.alignment")
        .and_then(|k| k.value.as_ref().and_then(|v| v.as_u64()))
        .filter(|a| *a > 0)
        .unwrap_or(DEFAULT_ALIGNMENT);
    let data_start = header_end.div_ceil(alignment) * alignment;
    let mut order: Vec<usize> = (0..raw.len()).collect();
    order.sort_by_key(|i| raw[*i].3);
    let mut sizes = vec![0u64; raw.len()];
    for (k, i) in order.iter().enumerate() {
        let (_, shape, t, off) = &raw[*i];
        let end = order
            .get(k + 1)
            .map_or(file_size.saturating_sub(data_start), |j| raw[*j].3);
        sizes[*i] = tensor_nbytes(*t, shape).unwrap_or(end.saturating_sub(*off));
    }
    let tensors = raw
        .into_iter()
        .enumerate()
        .map(|(i, (name, shape, ttype, offset))| TensorInfo {
            name,
            shape,
            ttype,
            offset,
            nbytes: sizes[i],
            part,
        })
        .collect();
    Ok(GgufFile {
        path: path.to_string_lossy().to_string(),
        version,
        kv,
        tensors,
        data_start,
        alignment,
        file_size,
    })
}

/// A model of one or more GGUF files (a split model is one model). Tensors keep the order of their files.
#[derive(Debug, Clone)]
pub struct Model {
    pub files: Vec<GgufFile>,
    pub tensors: HashMap<String, TensorInfo>,
    pub order: Vec<String>,
    /// without `split.*`: the output is one file
    pub kv: Vec<Kv>,
    pub meta: HashMap<String, serde_json::Value>,
}

impl Model {
    pub fn new(files: Vec<GgufFile>) -> R<Model> {
        let mut tensors = HashMap::new();
        let mut order = vec![];
        for f in &files {
            for t in &f.tensors {
                if tensors.contains_key(&t.name) {
                    return gerr(format!(
                        "tensor {} appears twice in {}",
                        t.name, files[0].path
                    ));
                }
                tensors.insert(t.name.clone(), t.clone());
                order.push(t.name.clone());
            }
        }
        let kv: Vec<Kv> = files[0]
            .kv
            .iter()
            .filter(|k| !k.key.starts_with("split."))
            .cloned()
            .collect();
        let meta = kv
            .iter()
            .filter_map(|k| k.value.clone().map(|v| (k.key.clone(), v)))
            .collect();
        Ok(Model {
            files,
            tensors,
            order,
            kv,
            meta,
        })
    }

    pub fn arch(&self) -> String {
        self.meta
            .get("general.architecture")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }

    pub fn number(&self, suffix: &str) -> Option<i64> {
        let v = self.meta.get(&format!("{}.{suffix}", self.arch()))?;
        v.as_i64()
            .or_else(|| v.as_f64().map(|f| f as i64))
            .filter(|_| !v.is_boolean())
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.file_size).sum()
    }

    /// (file, absolute offset, size) of a tensor's data.
    pub fn location(&self, name: &str) -> (String, u64, u64) {
        let t = &self.tensors[name];
        let f = &self.files[t.part];
        (f.path.clone(), f.data_start + t.offset, t.nbytes)
    }

    pub fn alignment(&self) -> u64 {
        self.files[0].alignment
    }
}

pub fn open_model(first: &Path) -> R<Model> {
    let parts = crate::gguf::split_parts(first);
    let missing: Vec<String> = parts
        .iter()
        .filter(|p| !p.exists())
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        })
        .collect();
    if !missing.is_empty() {
        return gerr(format!(
            "part(s) of the model are missing: {}",
            missing.join(", ")
        ));
    }
    let files = parts
        .iter()
        .enumerate()
        .map(|(i, p)| read_gguf(p, i))
        .collect::<R<Vec<_>>>()?;
    Model::new(files)
}

// ------------------------------------------------------------------------------------------------ encoding

pub fn encode_string(text: &str) -> Vec<u8> {
    let mut out = (text.len() as u64).to_le_bytes().to_vec();
    out.extend(text.as_bytes());
    out
}

/// The metadata with one integer scalar replaced by `value` (same type).
pub fn with_value(kv: &[Kv], key: &str, value: i64) -> R<Vec<Kv>> {
    let mut found = false;
    let out = kv
        .iter()
        .map(|k| {
            if k.key != key {
                return Ok(k.clone());
            }
            found = true;
            let raw: Vec<u8> = match k.vtype {
                T_U8 => vec![value as u8],
                T_I8 => vec![value as i8 as u8],
                T_U16 => (value as u16).to_le_bytes().to_vec(),
                T_I16 => (value as i16).to_le_bytes().to_vec(),
                T_U32 => (value as u32).to_le_bytes().to_vec(),
                T_I32 => (value as i32).to_le_bytes().to_vec(),
                T_U64 => (value as u64).to_le_bytes().to_vec(),
                T_I64 => value.to_le_bytes().to_vec(),
                _ => return gerr(format!("{key} is not a number")),
            };
            Ok(Kv {
                key: k.key.clone(),
                vtype: k.vtype,
                raw,
                value: Some(serde_json::json!(value)),
            })
        })
        .collect::<R<Vec<_>>>()?;
    if !found {
        return gerr(format!("the model has no {key}"));
    }
    Ok(out)
}

/// (name, shape, type, bytes) of an output tensor.
pub type Info = (String, Vec<u64>, u32, u64);

/// Offsets of tensors in the data section (each starts on the alignment) and the size of the section.
pub fn layout(tensors: &[Info], alignment: u64) -> (Vec<u64>, u64) {
    let mut offsets = vec![];
    let mut pos = 0;
    for (_, _, _, n) in tensors {
        offsets.push(pos);
        pos += n.div_ceil(alignment) * alignment;
    }
    (offsets, pos)
}

/// Header bytes (padded to the alignment) and the tensor offsets; a pure function of its arguments, which lets a build
/// be resumed (the header of the partial file is compared with the one computed again).
pub fn build_header(kv: &[Kv], tensors: &[Info], alignment: u64) -> (Vec<u8>, Vec<u64>) {
    let mut kv = kv.to_vec();
    if alignment != DEFAULT_ALIGNMENT && !kv.iter().any(|k| k.key == "general.alignment") {
        kv.push(Kv {
            key: "general.alignment".into(),
            vtype: T_U32,
            raw: (alignment as u32).to_le_bytes().to_vec(),
            value: Some(serde_json::json!(alignment)),
        });
    }
    let (offsets, _) = layout(tensors, alignment);
    let mut out = MAGIC.to_vec();
    out.extend(3u32.to_le_bytes());
    out.extend((tensors.len() as u64).to_le_bytes());
    out.extend((kv.len() as u64).to_le_bytes());
    for k in &kv {
        out.extend(k.encoded());
    }
    for ((name, shape, t, _), off) in tensors.iter().zip(&offsets) {
        out.extend(encode_string(name));
        out.extend((shape.len() as u32).to_le_bytes());
        for d in shape {
            out.extend(d.to_le_bytes());
        }
        out.extend(t.to_le_bytes());
        out.extend(off.to_le_bytes());
    }
    let pad = (alignment - (out.len() as u64 % alignment)) % alignment;
    out.extend(std::iter::repeat_n(0u8, pad as usize));
    (out, offsets)
}

/// Write a small GGUF from memory (tests).
pub fn write_simple(
    path: &Path,
    kv: &[Kv],
    tensors: &[(String, Vec<u64>, u32, Vec<u8>)],
    alignment: u64,
) -> std::io::Result<()> {
    let infos: Vec<Info> = tensors
        .iter()
        .map(|(n, s, t, d)| (n.clone(), s.clone(), *t, d.len() as u64))
        .collect();
    let (header, _) = build_header(kv, &infos, alignment);
    let mut f = File::create(path)?;
    f.write_all(&header)?;
    for (_, _, _, d) in tensors {
        f.write_all(d)?;
        let pad = (alignment - (d.len() as u64 % alignment)) % alignment;
        f.write_all(&vec![0u8; pad as usize])?;
    }
    Ok(())
}

pub fn kv_u32(key: &str, v: u32) -> Kv {
    Kv {
        key: key.into(),
        vtype: T_U32,
        raw: v.to_le_bytes().to_vec(),
        value: Some(serde_json::json!(v)),
    }
}

pub fn kv_str(key: &str, v: &str) -> Kv {
    Kv {
        key: key.into(),
        vtype: T_STRING,
        raw: encode_string(v),
        value: Some(serde_json::json!(v)),
    }
}

// ------------------------------------------------------------------------------------------------ copying

/// Raised inside a step when the user cancelled the job.
#[derive(Debug)]
pub struct Cancelled;

/// Call `f` with the bytes of (file, offset, length) in chunks.
pub fn for_chunks(
    path: &str,
    offset: u64,
    nbytes: u64,
    f: &mut dyn FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let mut file = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    let mut left = nbytes;
    let mut buf = vec![0u8; CHUNK.min(nbytes.max(1) as usize)];
    while left > 0 {
        let take = (buf.len() as u64).min(left) as usize;
        let n = file.read(&mut buf[..take]).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err(format!(
                "{path} ends before the data of a tensor does (truncated file?)"
            ));
        }
        f(&buf[..n])?;
        left -= n as u64;
    }
    Ok(())
}

/// The SHA-256 of a whole file, in chunks.
pub fn sha256_file(path: &Path, progress: &mut dyn FnMut(u64)) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        done += n as u64;
        progress(done);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn path_buf(s: &str) -> PathBuf {
    match s.strip_prefix("~/") {
        Some(r) => dirs::home_dir().unwrap_or_default().join(r),
        None => PathBuf::from(s),
    }
}

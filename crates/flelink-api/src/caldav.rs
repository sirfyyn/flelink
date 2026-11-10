//! iCloud calendars over CalDAV (RFC 4791), signed in with an app-specific password (Nr. 942: "Apple per App-Passwort").
//! Read only: FLE Link finds the user's calendars, asks each for the events of a window with recurrences expanded, and
//! hands them to the agent's mirror (calendars.rs, server.rs `calendar_sync`). No XML or iCalendar crate: the few
//! elements and properties needed are read by hand, tolerant of namespace prefixes and folded lines.
use base64::Engine;
use serde_json::{json, Value};
use std::time::Duration;

pub const ICLOUD: &str = "https://caldav.icloud.com";

const PRINCIPAL: &str = r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:current-user-principal/></d:prop></d:propfind>"#;
const HOME: &str = r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><c:calendar-home-set/></d:prop></d:propfind>"#;
const LIST: &str = r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:resourcetype/><d:displayname/><c:supported-calendar-component-set/></d:prop></d:propfind>"#;

fn query(from: &str, to: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><c:calendar-data><c:expand start="{from}" end="{to}"/></c:calendar-data></d:prop><c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"><c:time-range start="{from}" end="{to}"/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"#
    )
}

/// The contents of every element with this local name, whatever its namespace prefix.
fn elements(xml: &str, local: &str) -> Vec<String> {
    let mut out = vec![];
    let mut i = 0;
    while let Some(p) = xml[i..].find('<') {
        let s = i + p;
        let rest = &xml[s + 1..];
        if rest.starts_with(['/', '?', '!']) {
            i = s + 1;
            continue;
        }
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(rest.len());
        let qname = &rest[..end];
        let Some(gt) = rest.find('>') else {
            break;
        };
        if qname
            .rsplit(':')
            .next()
            .is_some_and(|l| l.eq_ignore_ascii_case(local))
        {
            if rest[..gt].ends_with('/') {
                out.push(String::new());
                i = s + 1 + gt;
                continue;
            }
            let body = s + 2 + gt;
            let close = format!("</{qname}>");
            match xml[body..].find(&close) {
                Some(c) => {
                    out.push(xml[body..body + c].to_string());
                    i = body + c + close.len();
                }
                None => break,
            }
            continue;
        }
        i = s + 1;
    }
    out
}

fn first(xml: &str, path: &[&str]) -> Option<String> {
    let mut cur = xml.to_string();
    for p in path {
        cur = elements(&cur, p).into_iter().next()?;
    }
    Some(unescape(cur.trim()))
}

fn unescape(s: &str) -> String {
    let s = s.trim();
    let s = s
        .strip_prefix("<![CDATA[")
        .and_then(|x| x.strip_suffix("]]>"))
        .unwrap_or(s);
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#13;", "\r")
        .replace("&#xD;", "\r")
        .replace("&amp;", "&")
}

/// An address the server named (`/123/principal/` or a whole URL) against the one it was asked at.
fn join(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    let origin = base
        .find("://")
        .map(|i| i + 3)
        .and_then(|i| base[i..].find('/').map(|j| &base[..i + j]))
        .unwrap_or(base);
    format!("{origin}{href}")
}

/// `2026-11-10T08:00:00Z` → `20261110T080000Z`, as CalDAV writes times.
fn dav_time(rfc3339: &str) -> String {
    rfc3339.replace(['-', ':'], "")
}

struct Dav {
    agent: ureq::Agent,
    auth: String,
}

impl Dav {
    fn new(user: &str, password: &str) -> Self {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(20))
                .redirects(0)
                .build(),
            auth: format!("Basic {token}"),
        }
    }

    /// One request, following up to four redirects by hand (iCloud sends each account to its own server).
    fn call(
        &self,
        method: &str,
        url: &str,
        depth: &str,
        body: &str,
    ) -> Result<(String, String), String> {
        let mut url = url.to_string();
        for _ in 0..5 {
            let answer = self
                .agent
                .request(method, &url)
                .set("Authorization", &self.auth)
                .set("Depth", depth)
                .set("Content-Type", "application/xml; charset=utf-8")
                .send_string(body);
            match answer {
                Ok(r) if (300..400).contains(&r.status()) => {
                    let to = r
                        .header("Location")
                        .ok_or("a redirect without an address")?;
                    url = join(&url, to);
                }
                Ok(r) => return Ok((url, r.into_string().map_err(|e| e.to_string())?)),
                Err(ureq::Error::Status(c, r)) if (300..400).contains(&c) => {
                    let to = r
                        .header("Location")
                        .ok_or("a redirect without an address")?;
                    url = join(&url, to);
                }
                Err(ureq::Error::Status(401, _)) => {
                    return Err(
                        "Apple did not accept the Apple ID or the app-specific password".into(),
                    )
                }
                Err(ureq::Error::Status(c, _)) => return Err(format!("iCloud answered HTTP {c}")),
                Err(e) => return Err(format!("iCloud is not reachable: {e}")),
            }
        }
        Err("iCloud sent too many redirects".into())
    }

    fn calendars(&self) -> Result<Vec<String>, String> {
        let (at, xml) = self.call("PROPFIND", &format!("{ICLOUD}/"), "0", PRINCIPAL)?;
        let principal = first(&xml, &["current-user-principal", "href"])
            .ok_or("iCloud named no account for this Apple ID")?;
        let (at, xml) = self.call("PROPFIND", &join(&at, &principal), "0", HOME)?;
        let home =
            first(&xml, &["calendar-home-set", "href"]).ok_or("iCloud named no calendars")?;
        let (at, xml) = self.call("PROPFIND", &join(&at, &home), "1", LIST)?;
        Ok(calendar_hrefs(&xml)
            .into_iter()
            .map(|h| join(&at, &h))
            .collect())
    }
}

/// The calendars in a listing that hold events (not reminders, not the inbox).
fn calendar_hrefs(xml: &str) -> Vec<String> {
    elements(xml, "response")
        .into_iter()
        .filter(|r| {
            let kind = elements(r, "resourcetype").join("");
            let comps = elements(r, "supported-calendar-component-set").join("");
            !elements(&kind, "calendar").is_empty()
                && (comps.is_empty() || comps.to_ascii_uppercase().contains("VEVENT"))
        })
        .filter_map(|r| first(&r, &["href"]))
        .collect()
}

/// Check the sign-in: true when Apple accepts it and there is at least one calendar.
pub fn check(user: &str, password: &str) -> Result<usize, String> {
    Dav::new(user, password).calendars().map(|c| c.len())
}

/// Every event in all of the user's calendars between two times (RFC 3339).
pub fn read(user: &str, password: &str, from: &str, to: &str) -> Result<Vec<Value>, String> {
    let dav = Dav::new(user, password);
    let body = query(&dav_time(from), &dav_time(to));
    let mut all = vec![];
    for cal in dav.calendars()? {
        let (_, xml) = dav.call("REPORT", &cal, "1", &body)?;
        for data in elements(&xml, "calendar-data") {
            all.extend(events(&unescape(&data)));
        }
    }
    Ok(all)
}

// ------------------------------------------------------------------------------------------------ iCalendar

/// Folded lines joined, as RFC 5545 asks.
fn unfold(ics: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for line in ics.split('\n').map(|l| l.trim_end_matches('\r')) {
        match line.strip_prefix([' ', '\t']) {
            Some(more) if !out.is_empty() => out.last_mut().unwrap().push_str(more),
            _ => out.push(line.to_string()),
        }
    }
    out
}

fn text_value(v: &str) -> String {
    v.replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

/// A DTSTART or DTEND as RFC 3339 in UTC, and whether it is a whole day.
fn ical_time(params: &str, value: &str) -> Option<(chrono::DateTime<chrono::Utc>, bool)> {
    use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
    if value.len() == 8
        || params.to_ascii_uppercase().contains("VALUE=DATE") && !value.contains('T')
    {
        let d = NaiveDate::parse_from_str(&value[..8.min(value.len())], "%Y%m%d").ok()?;
        return Some((Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?), true));
    }
    let naive = NaiveDateTime::parse_from_str(value.trim_end_matches('Z'), "%Y%m%dT%H%M%S").ok()?;
    if value.ends_with('Z') {
        return Some((Utc.from_utc_datetime(&naive), false));
    }
    let tzid = params
        .split(';')
        .find_map(|p| p.strip_prefix("TZID="))
        .map(|t| t.trim_matches('"'));
    let utc = match tzid.and_then(|t| t.parse::<chrono_tz::Tz>().ok()) {
        Some(tz) => tz
            .from_local_datetime(&naive)
            .earliest()?
            .with_timezone(&Utc),
        None => chrono::Local
            .from_local_datetime(&naive)
            .earliest()?
            .with_timezone(&Utc),
    };
    Some((utc, false))
}

/// `PT1H30M`, `P1D`: enough of RFC 5545 durations for event lengths.
fn duration(v: &str) -> Option<chrono::Duration> {
    let v = v.strip_prefix('P')?;
    let (days, time) = v.split_once('T').unwrap_or((v, ""));
    let num = |s: &str, unit: char| -> i64 {
        s.split_once(unit)
            .and_then(|(n, _)| n.rsplit(|c: char| !c.is_ascii_digit()).next()?.parse().ok())
            .unwrap_or(0)
    };
    let weeks = num(days, 'W');
    let d = num(days, 'D');
    Some(
        chrono::Duration::days(weeks * 7 + d)
            + chrono::Duration::hours(num(time, 'H'))
            + chrono::Duration::minutes(num(time, 'M'))
            + chrono::Duration::seconds(num(time, 'S')),
    )
}

/// The events of one calendar object, in the form the agent's mirror takes.
pub fn events(ics: &str) -> Vec<Value> {
    let fmt =
        |t: chrono::DateTime<chrono::Utc>| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut out = vec![];
    let mut cur: Option<Vec<(String, String, String)>> = None;
    for line in unfold(ics) {
        if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
            cur = Some(vec![]);
            continue;
        }
        if line.eq_ignore_ascii_case("END:VEVENT") {
            let Some(props) = cur.take() else { continue };
            let get = |n: &str| props.iter().find(|(k, _, _)| k == n);
            if get("STATUS").is_some_and(|(_, _, v)| v.eq_ignore_ascii_case("CANCELLED")) {
                continue;
            }
            let Some((_, p, v)) = get("DTSTART") else {
                continue;
            };
            let Some((start, all_day)) = ical_time(p, v) else {
                continue;
            };
            let end = match get("DTEND").and_then(|(_, p, v)| ical_time(p, v)) {
                Some((e, _)) => e,
                None => {
                    let len = get("DURATION").and_then(|(_, _, v)| duration(v));
                    start
                        + len.unwrap_or(if all_day {
                            chrono::Duration::days(1)
                        } else {
                            chrono::Duration::hours(1)
                        })
                }
            };
            let uid = get("UID").map(|(_, _, v)| v.clone()).unwrap_or_default();
            // the instances of a repeated event share their UID: the start tells them apart
            let id = format!("{uid}@{}", fmt(start));
            let text = |n: &str| get(n).map(|(_, _, v)| text_value(v)).unwrap_or_default();
            out.push(json!({
                "id": id, "title": text("SUMMARY"), "description": text("DESCRIPTION"), "location": text("LOCATION"),
                "from": fmt(start), "to": fmt(end), "all_day": all_day,
            }));
            continue;
        }
        if let Some(props) = cur.as_mut() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let (name, params) = key.split_once(';').unwrap_or((key, ""));
            props.push((
                name.to_ascii_uppercase(),
                params.to_string(),
                value.to_string(),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_are_found_whatever_their_prefix() {
        let xml = r#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>/1/principal/</D:href><D:propstat><D:prop><D:current-user-principal><D:href>/123/principal/</D:href></D:current-user-principal></D:prop></D:propstat></D:response></D:multistatus>"#;
        assert_eq!(
            first(xml, &["current-user-principal", "href"]).as_deref(),
            Some("/123/principal/")
        );
        let other = r#"<multistatus xmlns="DAV:"><response><propstat><prop><calendar-home-set xmlns="urn:ietf:params:xml:ns:caldav"><href xmlns="DAV:">https://p52-caldav.icloud.com:443/123/calendars/</href></calendar-home-set></prop></propstat></response></multistatus>"#;
        assert_eq!(
            first(other, &["calendar-home-set", "href"]).as_deref(),
            Some("https://p52-caldav.icloud.com:443/123/calendars/")
        );
    }

    #[test]
    fn only_event_calendars_are_read() {
        let xml = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
<d:response><d:href>/123/calendars/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
<d:response><d:href>/123/calendars/home/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype><c:supported-calendar-component-set><c:comp name="VEVENT"/></c:supported-calendar-component-set></d:prop></d:propstat></d:response>
<d:response><d:href>/123/calendars/tasks/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype><c:supported-calendar-component-set><c:comp name="VTODO"/></c:supported-calendar-component-set></d:prop></d:propstat></d:response>
</d:multistatus>"#;
        assert_eq!(calendar_hrefs(xml), ["/123/calendars/home/"]);
        assert_eq!(
            join(
                "https://p52-caldav.icloud.com:443/123/principal/",
                "/123/calendars/home/"
            ),
            "https://p52-caldav.icloud.com:443/123/calendars/home/"
        );
        assert_eq!(dav_time("2026-11-10T08:00:00Z"), "20261110T080000Z");
    }

    #[test]
    fn events_are_read_from_icalendar() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a1\r\nSUMMARY:Zahnarzt\\, kurz\r\nDESCRIPTION:Zeile eins\\nZeile\r\n  zwei\r\nDTSTART;TZID=Europe/Berlin:20261110T090000\r\nDTEND;TZID=Europe/Berlin:20261110T100000\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:b2\r\nSUMMARY:Urlaub\r\nDTSTART;VALUE=DATE:20261111\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:c3\r\nSTATUS:CANCELLED\r\nDTSTART:20261112T080000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:d4\r\nDTSTART:20261113T080000Z\r\nDURATION:PT1H30M\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let e = events(ics);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0]["title"], "Zahnarzt, kurz");
        assert_eq!(e[0]["description"], "Zeile eins\nZeile zwei");
        assert_eq!(e[0]["from"], "2026-11-10T08:00:00Z");
        assert_eq!(e[0]["to"], "2026-11-10T09:00:00Z");
        assert_eq!(e[1]["all_day"], true);
        assert_eq!(e[1]["to"], "2026-11-12T00:00:00Z");
        assert_eq!(e[2]["to"], "2026-11-13T09:30:00Z");
        assert_eq!(e[0]["id"], "a1@2026-11-10T08:00:00Z");
    }
}

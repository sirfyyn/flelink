//! Calendars from outside: FLE Link reads them with the connected account (accounts.rs) and hands the agent a mirror
//! (`POST /api/calendar/sync` of FLE Durable). One way for now: what happens there shows up here, not the other way round.
use serde_json::{json, Value};
use std::time::Duration;

const API: &str = "https://www.googleapis.com/calendar/v3/calendars/primary/events";
const MAX_PAGES: usize = 8;

/// How far the mirror looks, in days around today.
pub const DAYS_BACK: i64 = 7;
pub const DAYS_AHEAD: i64 = 90;

/// One day's date as a time; a Google all-day event ends on the day after it.
fn day(date: &str) -> String {
    format!("{date}T00:00:00Z")
}

/// Google's events as the agent's calendar takes them.
pub fn google_entries(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .filter(|e| e["status"].as_str() != Some("cancelled"))
        .filter_map(|e| {
            let (from, to, all_day) = match (
                e["start"]["dateTime"].as_str(),
                e["end"]["dateTime"].as_str(),
            ) {
                (Some(a), Some(b)) => (a.to_string(), b.to_string(), false),
                _ => (
                    day(e["start"]["date"].as_str()?),
                    day(e["end"]["date"].as_str()?),
                    true,
                ),
            };
            Some(json!({
                "id": e["id"], "title": e["summary"].as_str().unwrap_or(""),
                "description": e["description"].as_str().unwrap_or(""),
                "location": e["location"].as_str().unwrap_or(""),
                "from": from, "to": to, "all_day": all_day,
            }))
        })
        .collect()
}

/// Everything in the primary calendar between two times; the reason in words when it cannot be read.
pub fn google_read(token: &str, from: &str, to: &str) -> Result<Vec<Value>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build();
    let mut all = vec![];
    let mut page = String::new();
    for _ in 0..MAX_PAGES {
        let mut req = agent
            .get(API)
            .set("Authorization", &format!("Bearer {token}"))
            .query("singleEvents", "true")
            .query("orderBy", "startTime")
            .query("maxResults", "250")
            .query("timeMin", from)
            .query("timeMax", to);
        if !page.is_empty() {
            req = req.query("pageToken", &page);
        }
        let doc: Value = match req.call() {
            Ok(r) => r
                .into_string()
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .ok_or("Google sent no JSON")?,
            Err(ureq::Error::Status(c, _)) => return Err(format!("Google answered HTTP {c}")),
            Err(e) => return Err(format!("Google is not reachable: {e}")),
        };
        all.extend(google_entries(
            doc["items"].as_array().map_or(&[], Vec::as_slice),
        ));
        match doc["nextPageToken"].as_str() {
            Some(t) if !t.is_empty() => page = t.to_string(),
            _ => return Ok(all),
        }
    }
    Err("The calendar has more events than FLE Link reads at once".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_events_become_entries() {
        let items = [
            json!({"id": "a", "summary": "Zahnarzt", "status": "confirmed",
                "start": {"dateTime": "2026-11-10T09:00:00+01:00"}, "end": {"dateTime": "2026-11-10T10:00:00+01:00"}}),
            json!({"id": "b", "start": {"date": "2026-11-11"}, "end": {"date": "2026-11-12"}}),
            json!({"id": "c", "status": "cancelled", "start": {"dateTime": "x"}, "end": {"dateTime": "y"}}),
            json!({"id": "d", "summary": "no times"}),
        ];
        let e = google_entries(&items);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0]["title"], "Zahnarzt");
        assert_eq!(e[0]["all_day"], false);
        assert_eq!(e[1]["from"], "2026-11-11T00:00:00Z");
        assert_eq!(e[1]["to"], "2026-11-12T00:00:00Z");
        assert_eq!(e[1]["all_day"], true);
    }
}

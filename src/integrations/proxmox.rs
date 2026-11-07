//! The `proxmox` kind («Proxmox VE»): the nodes, guests and storage of a
//! Proxmox VE host or cluster, read through its API with an API token.
//!
//! One call a fetch: `GET /api2/json/cluster/resources`, which lists nodes,
//! guests and storage together (also on a single host). The token's role is
//! `PVEAuditor`, which is read-only.
//!
//! The token is sent as `Authorization: PVEAPIToken=<id>=<secret>`, marked
//! sensitive so it stays out of debug output and is not carried to another
//! host by a redirect. The id (`user@realm!tokenid`) is a setting; its shape
//! is checked, so nothing that could end a header line gets into the header.
//! The secret is the service's secret.
//!
//! Every field of the answer is optional: an odd or missing one costs only the
//! metric it feeds. Names (nodes) come from outside and are never put into a
//! need or a value; [`node_rows`] hands them to a page that shows them as text.
//!
//! A self-signed certificate is refused by the shared client; pinning one is
//! not part of this kind yet.

use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::header::HeaderValue;
use serde_json::Value as Json;

use super::api;
use super::{
    BoxFuture, FetchCtx, FetchError, Field, FieldKind, Format, Integration, Level, MetricDef, Need,
    SettingError, Snapshot, Status, Value,
};

/// The only instance.
pub static PROXMOX: ProxmoxKind = ProxmoxKind;

/// See the module.
pub struct ProxmoxKind;

/// From this much used a storage is a warning («Lagring full»)…
pub const STORAGE_WARN: f64 = 85.0;
/// …and from this much it is bad.
pub const STORAGE_BAD: f64 = 95.0;

/// The key of the need «a node is down».
pub const NODE_DOWN_NEED: &str = "pve:node_down";
/// The key of the need «a storage is nearly full».
pub const STORAGE_NEED: &str = "pve:storage";

/// Longest node name kept, in characters.
const NAME_CHARS: usize = 80;
/// Longest token id accepted, in characters.
const TOKEN_ID_CHARS: usize = 128;

static FIELDS: [Field; 2] = [
    Field {
        key: "token_id",
        label_key: "field.pve_token_id",
        help_key: "field.pve_token_id_help",
        placeholder: "magpie@pve!start",
        kind: FieldKind::Text,
        required: true,
        secret: false,
    },
    Field {
        key: "secret",
        label_key: "field.pve_secret",
        help_key: "field.pve_secret_help",
        placeholder: "",
        kind: FieldKind::Text,
        required: true,
        secret: true,
    },
];

static METRICS: [MetricDef; 7] = [
    MetricDef {
        id: "nodes_online",
        label_key: "metric.pve_nodes_online",
        format: Format::Number,
    },
    MetricDef {
        id: "nodes_total",
        label_key: "metric.pve_nodes_total",
        format: Format::Number,
    },
    MetricDef {
        id: "guests_running",
        label_key: "metric.pve_guests_running",
        format: Format::Number,
    },
    MetricDef {
        id: "guests_total",
        label_key: "metric.pve_guests_total",
        format: Format::Number,
    },
    MetricDef {
        id: "cpu",
        label_key: "metric.pve_cpu",
        format: Format::Percent,
    },
    MetricDef {
        id: "ram",
        label_key: "metric.pve_ram",
        format: Format::Percent,
    },
    MetricDef {
        id: "storage_max",
        label_key: "metric.pve_storage_max",
        format: Format::Percent,
    },
];

/// One node, for a page that lists the nodes. Each figure is a percentage and
/// is `None` when the answer has no usable number for it (an offline node
/// has none).
#[derive(Debug, Clone, PartialEq)]
pub struct NodeRow {
    /// The node's name, cleaned (it came from outside).
    pub name: String,
    pub online: bool,
    pub cpu: Option<f64>,
    pub ram: Option<f64>,
    /// The node's own (root) disk.
    pub disk: Option<f64>,
}

/// What is read of an answer. Each figure is missing when the answer does not
/// have it in a usable form.
#[derive(Debug, Default, PartialEq)]
struct Reading {
    nodes_online: f64,
    nodes_offline: f64,
    nodes_total: f64,
    guests_running: f64,
    guests_total: f64,
    cpu: Option<f64>,
    ram: Option<f64>,
    storage_max: Option<f64>,
}

/// Whether `id` looks like `user@realm!tokenid`: three non-empty parts of
/// ASCII letters, digits, `.`, `_` and `-`. Nothing else, so the id can go
/// into a header and a URL-free text safely.
pub fn token_id_ok(id: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    let Some((user_realm, token)) = id.split_once('!') else {
        return false;
    };
    let Some((user, realm)) = user_realm.split_once('@') else {
        return false;
    };
    id.chars().count() <= TOKEN_ID_CHARS && part(user) && part(realm) && part(token)
}

/// `PVEAPIToken=<id>=<secret>` as a sensitive header value. A missing or
/// malformed id, or a missing secret or one that cannot be a header value, is
/// [`FetchError::Config`].
fn auth(settings: &Json, secret: Option<&str>) -> Result<HeaderValue, FetchError> {
    let id = settings
        .get("token_id")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|id| token_id_ok(id))
        .ok_or(FetchError::Config)?;
    let secret = secret.filter(|s| !s.is_empty()).ok_or(FetchError::Config)?;
    let mut value = HeaderValue::from_str(&format!("PVEAPIToken={id}={secret}"))
        .map_err(|_| FetchError::Config)?;
    value.set_sensitive(true);
    Ok(value)
}

/// The text of `item[key]`, when it is one.
fn text<'a>(item: &'a Json, key: &str) -> Option<&'a str> {
    item.get(key).and_then(Json::as_str)
}

/// A finite number that is not below 0.
fn amount(value: Option<&Json>) -> Option<f64> {
    value
        .and_then(Json::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
}

/// `used` of `max` as a percentage (0..=100); `None` when either is odd or
/// `max` is not above 0.
fn ratio(used: Option<&Json>, max: Option<&Json>) -> Option<f64> {
    let (used, max) = (amount(used)?, amount(max)?);
    (max > 0.0).then(|| (used / max * 100.0).clamp(0.0, 100.0))
}

/// The entries of the answer's `data` list that are objects.
fn entries(doc: &Json) -> Option<impl Iterator<Item = &Json>> {
    let list = doc.get("data")?.as_array()?;
    Some(list.iter().filter(|item| item.is_object()))
}

/// A node's CPU use as a percentage (the answer has a fraction of 1).
fn cpu_percent(node: &Json) -> Option<f64> {
    amount(node.get("cpu")).map(|f| (f * 100.0).clamp(0.0, 100.0))
}

/// The nodes of the answer as rows, in the order the answer has them. A node
/// without a usable name is left out. Not a need and not stored: a page that
/// lists the nodes reads it from the answer.
pub fn node_rows(doc: &Json) -> Vec<NodeRow> {
    let Some(items) = entries(doc) else {
        return Vec::new();
    };
    items
        .filter(|item| text(item, "type") == Some("node"))
        .filter_map(|node| {
            let name = text(node, "node").or_else(|| text(node, "name"))?;
            let name = api::clean_text(name, NAME_CHARS);
            (!name.is_empty()).then(|| NodeRow {
                name,
                online: text(node, "status") == Some("online"),
                cpu: cpu_percent(node),
                ram: ratio(node.get("mem"), node.get("maxmem")),
                disk: ratio(node.get("disk"), node.get("maxdisk")),
            })
        })
        .collect()
}

/// Whether a guest entry is a template (`template` is 1, or true).
fn is_template(item: &Json) -> bool {
    match item.get("template") {
        Some(Json::Bool(b)) => *b,
        Some(other) => other.as_f64() == Some(1.0),
        None => false,
    }
}

/// The reading of a `cluster/resources` answer. Without a `data` list, or
/// without one node in it, there is nothing to show: [`FetchError::Invalid`]
/// (a token that may see nothing gets an empty list).
fn read_answer(doc: &Json) -> Result<Reading, FetchError> {
    let items: Vec<&Json> = entries(doc).ok_or(FetchError::Invalid)?.collect();
    let of_type = |kind: &'static str| {
        items
            .iter()
            .copied()
            .filter(move |item| text(item, "type") == Some(kind))
    };

    let nodes: Vec<&Json> = of_type("node").collect();
    if nodes.is_empty() {
        return Err(FetchError::Invalid);
    }
    let nodes_online = nodes
        .iter()
        .filter(|n| text(n, "status") == Some("online"))
        .count();
    let nodes_offline = nodes
        .iter()
        .filter(|n| text(n, "status") == Some("offline"))
        .count();

    let cpus: Vec<f64> = nodes.iter().filter_map(|n| cpu_percent(n)).collect();
    let cpu = (!cpus.is_empty()).then(|| cpus.iter().sum::<f64>() / cpus.len() as f64);

    // Σmem / Σmaxmem over the nodes that report both.
    let (mem, max_mem) = nodes
        .iter()
        .filter_map(|n| Some((amount(n.get("mem"))?, amount(n.get("maxmem"))?)))
        .fold((0.0, 0.0), |(m, x), (mem, max)| (m + mem, x + max));
    let ram = (max_mem > 0.0).then(|| (mem / max_mem * 100.0).clamp(0.0, 100.0));

    let guests: Vec<&Json> = of_type("qemu")
        .chain(of_type("lxc"))
        .filter(|g| !is_template(g))
        .collect();
    let guests_running = guests
        .iter()
        .filter(|g| text(g, "status") == Some("running"))
        .count();

    // The fullest storage that is in use: one that says it is not available
    // (disabled, unknown) says nothing about space. A shared storage appears
    // once per node, with the same figures; the maximum is the same either way.
    let storage_max = of_type("storage")
        .filter(|s| text(s, "status").is_none_or(|status| status == "available"))
        .filter_map(|s| ratio(s.get("disk"), s.get("maxdisk")))
        .max_by(f64::total_cmp);

    Ok(Reading {
        nodes_online: nodes_online as f64,
        nodes_offline: nodes_offline as f64,
        nodes_total: nodes.len() as f64,
        guests_running: guests_running as f64,
        guests_total: guests.len() as f64,
        cpu,
        ram,
        storage_max,
    })
}

fn snapshot(reading: &Reading) -> Snapshot {
    let storage_level = reading.storage_max.and_then(|pct| {
        if pct >= STORAGE_BAD {
            Some(Level::Bad)
        } else if pct >= STORAGE_WARN {
            Some(Level::Warn)
        } else {
            None
        }
    });
    let status = if reading.nodes_offline > 0.0 {
        Status::Bad
    } else if storage_level.is_some() {
        Status::Warn
    } else {
        Status::Ok
    };
    let mut snapshot = Snapshot::new(status);
    let mut values = BTreeMap::new();
    for (id, number) in [
        ("nodes_online", Some(reading.nodes_online)),
        ("nodes_total", Some(reading.nodes_total)),
        ("guests_running", Some(reading.guests_running)),
        ("guests_total", Some(reading.guests_total)),
        ("cpu", reading.cpu),
        ("ram", reading.ram),
        ("storage_max", reading.storage_max),
    ] {
        if let Some(number) = number {
            values.insert(id.to_string(), Value::num(number));
        }
    }
    snapshot.values = values;
    if reading.nodes_offline > 0.0 {
        snapshot.needs.push(need(
            NODE_DOWN_NEED,
            Level::Bad,
            "need.pve_node_down",
            reading.nodes_offline,
            reading.nodes_offline,
        ));
    }
    if let (Some(level), Some(pct)) = (storage_level, reading.storage_max) {
        // Rounded down, so the number shown is never above the real one.
        snapshot.needs.push(need(
            STORAGE_NEED,
            level,
            "need.pve_storage",
            pct.floor(),
            pct,
        ));
    }
    snapshot
}

/// A need whose text takes only a count (the card shows the service's name
/// next to it); no name from outside goes in.
fn need(key: &str, level: Level, label_key: &str, count: f64, score: f64) -> Need {
    Need {
        key: key.to_string(),
        level,
        label_key: label_key.to_string(),
        params: BTreeMap::from([("count".to_string(), format!("{count:.0}"))]),
        score,
        since: None,
    }
}

async fn read(ctx: &FetchCtx) -> Result<Snapshot, FetchError> {
    let base = api::base_url(&ctx.settings)?;
    let auth = auth(&ctx.settings, ctx.secret.as_deref())?;
    let url = api::endpoint(&base, &["api2", "json", "cluster", "resources"], None)?;
    let doc: Json = api::get_json(&ctx.http, url, &auth).await?.found()?;
    read_answer(&doc).map(|reading| snapshot(&reading))
}

impl Integration for ProxmoxKind {
    fn kind(&self) -> &'static str {
        "proxmox"
    }

    fn label_key(&self) -> &'static str {
        "kind.proxmox"
    }

    fn fields(&self) -> &'static [Field] {
        &FIELDS
    }

    fn metrics(&self) -> &'static [MetricDef] {
        &METRICS
    }

    fn default_interval(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn min_interval(&self) -> Duration {
        Duration::from_secs(15)
    }

    fn check_settings(&self, settings: &serde_json::Map<String, Json>) -> Result<(), SettingError> {
        match settings.get("token_id").and_then(Json::as_str) {
            Some(id) if !token_id_ok(id.trim()) => Err(SettingError {
                field: "token_id",
                message_key: "err.pve_token_id",
            }),
            _ => Ok(()),
        }
    }

    fn fetch<'a>(&'a self, ctx: &'a FetchCtx) -> BoxFuture<'a, Result<Snapshot, FetchError>> {
        Box::pin(read(ctx))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn doc() -> Json {
        json!({ "data": [
            { "id": "node/pve1", "type": "node", "node": "pve1", "status": "online",
              "cpu": 0.10, "maxcpu": 8, "mem": 4_000, "maxmem": 8_000, "disk": 10, "maxdisk": 100 },
            { "id": "node/pve2", "type": "node", "node": "pve2", "status": "online",
              "cpu": 0.30, "maxcpu": 8, "mem": 2_000, "maxmem": 8_000, "disk": 50, "maxdisk": 100 },
            { "id": "node/pve3", "type": "node", "node": "pve3", "status": "offline" },
        ]})
    }

    #[test]
    fn node_rows_have_one_row_per_node_with_percentages() {
        let rows = node_rows(&doc());
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            NodeRow {
                name: "pve1".into(),
                online: true,
                cpu: Some(10.0),
                ram: Some(50.0),
                disk: Some(10.0),
            }
        );
        assert_eq!(rows[1].ram, Some(25.0));
        assert_eq!(rows[1].disk, Some(50.0));
        assert!((rows[1].cpu.unwrap() - 30.0).abs() < 1e-9);
        // An offline node is a row with no figures.
        assert_eq!(
            rows[2],
            NodeRow {
                name: "pve3".into(),
                online: false,
                cpu: None,
                ram: None,
                disk: None,
            }
        );
    }

    #[test]
    fn node_rows_ignore_guests_and_storage_and_odd_data() {
        let doc = json!({ "data": [
            { "type": "qemu", "node": "pve1", "vmid": 100 },
            { "type": "storage", "node": "pve1", "storage": "local" },
            "text", 3, null,
            { "type": "node", "node": "a\u{202E}b\nc", "status": "online",
              "cpu": "x", "mem": -1, "maxmem": 0, "disk": 5, "maxdisk": 0 },
            { "type": "node", "status": "online" },
            { "type": "node", "node": "   ", "status": "online" },
            { "type": "node", "name": "alt", "status": 3, "cpu": 2.5 },
        ]});
        let rows = node_rows(&doc);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "ab c");
        assert!(rows[0].online);
        assert_eq!((rows[0].cpu, rows[0].ram, rows[0].disk), (None, None, None));
        assert_eq!(rows[1].name, "alt");
        assert!(!rows[1].online);
        // A fraction above 1 is clamped.
        assert_eq!(rows[1].cpu, Some(100.0));
        for odd in [
            json!({}),
            json!({ "data": null }),
            json!({ "data": {} }),
            json!(3),
        ] {
            assert!(node_rows(&odd).is_empty());
        }
    }

    #[test]
    fn the_token_id_has_to_be_three_plain_parts() {
        for good in [
            "root@pam!magpie",
            "magpie@pve!start-1",
            "a.b_c@my-realm!t.1",
        ] {
            assert!(token_id_ok(good), "{good}");
        }
        for bad in [
            "",
            "root@pam",
            "root!magpie",
            "@pam!x",
            "root@!x",
            "root@pam!",
            "root@pam!a!b",
            "a@b@c!d",
            "root@pam!ma gpie",
            "root@pam!x\r\nX-Evil: 1",
            "root@pam!x=y",
            "root@pam!ø",
        ] {
            assert!(!token_id_ok(bad), "{bad:?}");
        }
        assert!(!token_id_ok(&format!("a@b!{}", "c".repeat(200))));
    }

    #[test]
    fn the_header_is_sensitive_and_checked() {
        let settings = json!({ "token_id": " root@pam!magpie " });
        let value = auth(&settings, Some("1234-abcd")).unwrap();
        assert!(value.is_sensitive());
        assert_eq!(
            value.to_str().unwrap(),
            "PVEAPIToken=root@pam!magpie=1234-abcd"
        );
        assert!(!format!("{value:?}").contains("1234-abcd"));
        for secret in [None, Some(""), Some("a\r\nb")] {
            assert_eq!(auth(&settings, secret).unwrap_err(), FetchError::Config);
        }
        for settings in [
            json!({}),
            json!({ "token_id": 3 }),
            json!({ "token_id": "x\ny@z!w" }),
        ] {
            assert_eq!(auth(&settings, Some("s")).unwrap_err(), FetchError::Config);
        }
    }

    #[test]
    fn a_reading_counts_nodes_guests_and_the_means() {
        let mut data = doc();
        data["data"].as_array_mut().unwrap().extend([
            json!({ "type": "qemu", "vmid": 100, "status": "running", "template": 0 }),
            json!({ "type": "qemu", "vmid": 101, "status": "stopped" }),
            json!({ "type": "qemu", "vmid": 102, "status": "stopped", "template": 1 }),
            json!({ "type": "lxc", "vmid": 110, "status": "running" }),
            json!({ "type": "storage", "storage": "local", "node": "pve1", "status": "available",
                    "disk": 40, "maxdisk": 100 }),
            json!({ "type": "storage", "storage": "big", "node": "pve1", "status": "available",
                    "disk": 90, "maxdisk": 100 }),
            json!({ "type": "storage", "storage": "gone", "node": "pve1", "status": "unknown",
                    "disk": 99, "maxdisk": 100 }),
        ]);
        let r = read_answer(&data).unwrap();
        assert_eq!(
            (r.nodes_online, r.nodes_offline, r.nodes_total),
            (2.0, 1.0, 3.0)
        );
        assert_eq!((r.guests_running, r.guests_total), (2.0, 3.0));
        assert!((r.cpu.unwrap() - 20.0).abs() < 1e-9);
        assert_eq!(r.ram, Some(37.5));
        assert_eq!(r.storage_max, Some(90.0));
    }

    #[test]
    fn no_nodes_is_not_a_reading() {
        for doc in [
            json!({}),
            json!({ "data": null }),
            json!({ "data": {} }),
            json!({ "data": [] }),
            json!({ "data": [{ "type": "qemu", "status": "running" }] }),
        ] {
            assert_eq!(read_answer(&doc).unwrap_err(), FetchError::Invalid);
        }
    }

    #[test]
    fn the_status_and_the_needs_follow_the_nodes_and_the_storage() {
        let reading = |offline: f64, storage: Option<f64>| Reading {
            nodes_online: 1.0,
            nodes_offline: offline,
            nodes_total: 1.0 + offline,
            storage_max: storage,
            ..Reading::default()
        };
        let s = snapshot(&reading(0.0, Some(84.9)));
        assert_eq!((s.status, s.needs.len()), (Status::Ok, 0));
        let s = snapshot(&reading(0.0, Some(85.0)));
        assert_eq!(s.status, Status::Warn);
        assert_eq!(s.needs[0].level, Level::Warn);
        let s = snapshot(&reading(0.0, Some(95.9)));
        assert_eq!(s.status, Status::Warn);
        assert_eq!(s.needs[0].level, Level::Bad);
        assert_eq!(s.needs[0].params["count"], "95");
        assert_eq!(s.needs[0].score, 95.9);
        let s = snapshot(&reading(2.0, Some(96.0)));
        assert_eq!(s.status, Status::Bad);
        let keys: Vec<_> = s.needs.iter().map(|n| n.key.as_str()).collect();
        assert_eq!(keys, [NODE_DOWN_NEED, STORAGE_NEED]);
        assert_eq!(s.needs[0].params["count"], "2");
        assert_eq!(s.needs[0].score, 2.0);
    }

    #[test]
    fn odd_fields_only_drop_their_own_metric() {
        let doc = json!({ "data": [
            { "type": "node", "node": "a", "status": "online", "cpu": "x", "mem": 1, "maxmem": 2,
              "disk": 1, "maxdisk": 2 },
            { "type": "node", "node": "b", "status": "online", "cpu": null, "mem": -1, "maxmem": 2 },
            { "type": "storage", "status": "available", "disk": 1, "maxdisk": 0 },
            { "type": "storage", "status": "available", "disk": "x", "maxdisk": 10 },
            { "type": "qemu", "template": "yes" },
        ]});
        let r = read_answer(&doc).unwrap();
        assert_eq!(r.cpu, None);
        assert_eq!(r.ram, Some(50.0));
        assert_eq!(r.storage_max, None);
        assert_eq!((r.guests_total, r.guests_running), (1.0, 0.0));
        let ids: Vec<_> = snapshot(&r).values.keys().cloned().collect();
        assert_eq!(
            ids,
            [
                "guests_running",
                "guests_total",
                "nodes_online",
                "nodes_total",
                "ram"
            ]
        );
    }
}

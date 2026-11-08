//! The `proxmox` kind («Proxmox VE»): the nodes, guests and storage of a
//! Proxmox VE host or cluster, read through its API with an API token.
//!
//! One call a fetch: `GET /api2/json/cluster/resources`, which lists nodes,
//! guests and storage together (also on a single host). The token's role is
//! `PVEAuditor`, which is read-only.
//!
//! The token is sent as `Authorization: PVEAPIToken=<id>=<secret>`, marked
//! sensitive so it stays out of debug output; a redirect to another origin is
//! refused. The id (`user@realm!tokenid`) is a setting; its shape
//! is checked, so nothing that could end a header line gets into the header.
//! The secret is the service's secret.
//!
//! Every field of the answer is optional: an odd or missing one costs only the
//! metric it feeds. Names (nodes, guests, storage) come from outside and are
//! never put into a need or a value; [`node_rows`] hands the nodes to a page
//! that shows them as text, and [`details`] puts nodes, guests and storage into
//! detail blocks (bars and lists), which show them as text too. The blocks come
//! from the same answer: no further call.
//!
//! A server with a self-signed certificate is reached by pinning its
//! fingerprint in the service's settings, like for every kind (see
//! [`super::pin`]); the shared client refuses it otherwise.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value as Json;

use super::api;
use super::block::{Bar, Block, Fact, ListItem, MAX_ITEMS};
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
        peak: false,
    },
    MetricDef {
        id: "nodes_total",
        label_key: "metric.pve_nodes_total",
        format: Format::Number,
        peak: false,
    },
    MetricDef {
        id: "guests_running",
        label_key: "metric.pve_guests_running",
        format: Format::Number,
        peak: false,
    },
    MetricDef {
        id: "guests_total",
        label_key: "metric.pve_guests_total",
        format: Format::Number,
        peak: false,
    },
    MetricDef {
        id: "cpu",
        label_key: "metric.pve_cpu",
        format: Format::Percent,
        peak: false,
    },
    MetricDef {
        id: "ram",
        label_key: "metric.pve_ram",
        format: Format::Percent,
        peak: false,
    },
    MetricDef {
        id: "storage_max",
        label_key: "metric.pve_storage_max",
        format: Format::Percent,
        peak: false,
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

/// `Authorization: PVEAPIToken=<id>=<secret>`. A missing or malformed id, or
/// a missing secret or one that cannot be a header value, is
/// [`FetchError::Config`].
fn auth(settings: &Json, secret: Option<&str>) -> Result<api::Credential, FetchError> {
    let id = settings
        .get("token_id")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|id| token_id_ok(id))
        .ok_or(FetchError::Config)?;
    let secret = secret.filter(|s| !s.is_empty()).ok_or(FetchError::Config)?;
    api::Credential::raw("authorization", &format!("PVEAPIToken={id}={secret}"))
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

/// The reading of a `cluster/resources` answer. Without a `data` list there
/// is nothing to show: [`FetchError::Invalid`]. A list with no node in it is
/// what a token that may see nothing gets: [`FetchError::NothingVisible`].
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
        return Err(FetchError::NothingVisible);
    }
    let nodes_online = nodes
        .iter()
        .filter(|n| text(n, "status") == Some("online"))
        .count();
    let nodes_offline = nodes
        .iter()
        .filter(|n| text(n, "status") == Some("offline"))
        .count();

    // An offline node reports no load (or the last one it had): CPU and memory
    // are those of the nodes that are online.
    let online: Vec<&&Json> = nodes
        .iter()
        .filter(|n| text(n, "status") == Some("online"))
        .collect();
    let cpus: Vec<f64> = online.iter().filter_map(|n| cpu_percent(n)).collect();
    let cpu = (!cpus.is_empty()).then(|| cpus.iter().sum::<f64>() / cpus.len() as f64);

    // Σmem / Σmaxmem over the online nodes that report both.
    let (mem, max_mem) = online
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

/// Longest guest, node or storage name kept in a block line, in characters
/// (the id or the node that goes after it still fits in a line).
const LINE_NAME_CHARS: usize = 48;

/// Longest name or node in the label of a storage that has both, in
/// characters.
const LABEL_PART_CHARS: usize = 38;

const MIB: f64 = 1024.0 * 1024.0;
const GIB: f64 = 1024.0 * MIB;
const TIB: f64 = 1024.0 * GIB;

/// A number with one decimal, and none when it is whole («62», «25.6»).
fn short_number(n: f64) -> String {
    // A figure past what is a count of anything real is cut, so that the text
    // stays short.
    let n = n.min(api::MAX_COUNT);
    let rounded = (n * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.1}")
    }
}

/// The unit for a size and what to divide by: MiB, GiB or TiB (powers of 1024,
/// the way Proxmox shows them).
fn unit_for(total: f64) -> (f64, &'static str) {
    if total >= TIB {
        (TIB, "TiB")
    } else if total >= GIB {
        (GIB, "GiB")
    } else {
        (MIB, "MiB")
    }
}

/// «25.6 / 62 GiB»: `used` of `total` bytes, both in the unit of the total.
fn used_of(used: f64, total: f64) -> String {
    let (div, unit) = unit_for(total);
    format!(
        "{} / {} {unit}",
        short_number(used / div),
        short_number(total / div)
    )
}

/// «8 GiB»: a size in bytes.
fn size_text(bytes: f64) -> String {
    let (div, unit) = unit_for(bytes);
    format!("{} {unit}", short_number(bytes / div))
}

/// A bar of `used` of `max` bytes (the fields named), when the answer has
/// both and `max` is above 0.
fn byte_bar(label: String, item: &Json, used: &str, max: &str) -> Option<Bar> {
    let (used, max) = (amount(item.get(used))?, amount(item.get(max))?);
    (max > 0.0).then(|| Bar {
        label,
        pct: (used / max * 100.0).clamp(0.0, 100.0),
        text: used_of(used, max),
        ..Default::default()
    })
}

/// `bar` coloured as a disk is: yellow from [`STORAGE_WARN`] and red from
/// [`STORAGE_BAD`] (the thresholds of the storage needs), neutral below.
fn disk_fill(mut bar: Bar) -> Bar {
    bar.status = if bar.pct >= STORAGE_BAD {
        Some(Status::Bad)
    } else if bar.pct >= STORAGE_WARN {
        Some(Status::Warn)
    } else {
        None
    };
    bar
}

/// Most nodes a block shows when the disk bars have gone too: two bars each.
const MAX_NODES_SHORT: usize = MAX_ITEMS / 2;

/// The blocks of the nodes that are up: bars of CPU, RAM and the root disk for
/// each, with the figures in the text. A cluster so big that this is more than
/// a block holds gets CPU and RAM only (a shorter title says so), and one with
/// more nodes than fit even then shows the first few by name and a fact with
/// how many are left out.
fn node_blocks(doc: &Json) -> Vec<Block> {
    let Some(items) = entries(doc) else {
        return Vec::new();
    };
    let mut nodes: Vec<(String, &Json)> = items
        .filter(|item| text(item, "type") == Some("node"))
        .filter(|item| text(item, "status") == Some("online"))
        .filter_map(|node| {
            let name = text(node, "node").or_else(|| text(node, "name"))?;
            let name = api::clean_text(name, LINE_NAME_CHARS);
            (!name.is_empty()).then_some((name, node))
        })
        .collect();
    let many = nodes.len() > 1;
    let build = |nodes: &[(String, &Json)], with_disk: bool| -> Vec<Bar> {
        let mut bars = Vec::new();
        for (name, node) in nodes {
            let label = |what: &str| {
                if many {
                    format!("{name} {what}")
                } else {
                    what.to_string()
                }
            };
            if let Some(pct) = cpu_percent(node) {
                let cores = amount(node.get("maxcpu"))
                    .filter(|n| *n > 0.0)
                    .map(|n| format!(" · {} CPU", short_number(n)))
                    .unwrap_or_default();
                bars.push(Bar {
                    label: label("CPU"),
                    pct,
                    text: format!("{} %{cores}", short_number(pct)),
                    ..Default::default()
                });
            }
            bars.extend(byte_bar(label("RAM"), node, "mem", "maxmem"));
            if with_disk {
                bars.extend(byte_bar(label("Disk"), node, "disk", "maxdisk").map(disk_fill));
            }
        }
        bars
    };
    let mut blocks = Vec::new();
    let full = build(&nodes, true);
    let (title_key, bars) = if full.len() <= MAX_ITEMS {
        ("pve_nodes", full)
    } else {
        let mut bars = build(&nodes, false);
        if bars.len() > MAX_ITEMS {
            nodes.sort_by(|a, b| a.0.cmp(&b.0));
            let left_out = nodes.len().saturating_sub(MAX_NODES_SHORT);
            nodes.truncate(MAX_NODES_SHORT);
            bars = build(&nodes, false);
            if left_out > 0 {
                blocks.push(Block::Facts {
                    title_key: "pve_nodes".into(),
                    rows: vec![Fact {
                        label_key: Some("pve_nodes_more".into()),
                        label: None,
                        value: left_out.to_string(),
                        ..Default::default()
                    }],
                });
            }
        }
        ("pve_nodes_short", bars)
    };
    if !bars.is_empty() {
        blocks.insert(
            0,
            Block::Bars {
                title_key: title_key.into(),
                bars,
            },
        );
    }
    blocks
}

/// A guest of the answer.
struct Guest<'a> {
    name: Option<String>,
    /// `VM` or `CT`.
    kind: &'static str,
    id: Option<u64>,
    item: &'a Json,
}

impl Guest<'_> {
    /// «web (VM 100)»: the name (cleaned and short) and what it is.
    fn title(&self) -> String {
        let tag = match self.id {
            Some(id) => format!("{} {id}", self.kind),
            None => self.kind.to_string(),
        };
        match &self.name {
            Some(name) => format!("{name} ({tag})"),
            None => tag,
        }
    }

    fn cpu(&self) -> Option<f64> {
        cpu_percent(self.item)
    }

    /// «4 vCPU · 11 / 32 GiB RAM»: what it has and, when it runs, what it uses.
    fn sub(&self, running: bool) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(cpus) = amount(self.item.get("maxcpu")).filter(|n| *n > 0.0) {
            parts.push(format!("{} vCPU", short_number(cpus)));
        }
        let max = amount(self.item.get("maxmem")).filter(|n| *n > 0.0);
        match (max, amount(self.item.get("mem"))) {
            (Some(max), Some(mem)) if running => parts.push(format!("{} RAM", used_of(mem, max))),
            (Some(max), _) => parts.push(format!("{} RAM", size_text(max))),
            _ => {}
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

/// The guests of the answer that run (or, with `running` false, that do not);
/// templates are left out. In the order of their ids.
fn guests(doc: &Json, running: bool) -> Vec<Guest<'_>> {
    let Some(items) = entries(doc) else {
        return Vec::new();
    };
    let mut list: Vec<Guest<'_>> = items
        .filter_map(|item| {
            let kind = match text(item, "type")? {
                "qemu" => "VM",
                "lxc" => "CT",
                _ => return None,
            };
            if is_template(item) || (text(item, "status") == Some("running")) != running {
                return None;
            }
            Some(Guest {
                name: text(item, "name")
                    .map(|n| api::clean_text(n, LINE_NAME_CHARS))
                    .filter(|n| !n.is_empty()),
                kind,
                id: item.get("vmid").and_then(Json::as_u64),
                item,
            })
        })
        .collect();
    list.sort_by_key(|g| g.id);
    list
}

/// «Kjører»: the running guests, busiest first.
fn running_list(doc: &Json) -> Option<Block> {
    let mut list = guests(doc, true);
    // Highest CPU first; a guest with no figure goes last; ties keep the id order.
    list.sort_by(|a, b| match (a.cpu(), b.cpu()) {
        (Some(a), Some(b)) => b.total_cmp(&a),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let items: Vec<ListItem> = list
        .iter()
        .map(|g| ListItem {
            text: g.title(),
            sub: g.sub(true),
            status: Some(Status::Ok),
            right: g
                .cpu()
                .map(|pct| format!("{} % CPU", short_number(pct.round()))),
            href: None,
            ..Default::default()
        })
        .collect();
    (!items.is_empty()).then(|| Block::List {
        title_key: "pve_running".into(),
        items,
    })
}

/// «Stoppet»: the stopped guests, greyed (status unknown).
fn stopped_list(doc: &Json) -> Option<Block> {
    let items: Vec<ListItem> = guests(doc, false)
        .iter()
        .map(|g| ListItem {
            text: g.title(),
            sub: g.sub(false),
            status: Some(Status::Unknown),
            right: None,
            href: None,
            ..Default::default()
        })
        .collect();
    (!items.is_empty()).then(|| Block::List {
        title_key: "pve_stopped".into(),
        items,
    })
}

/// «Lagring»: the storages that are in use, fullest first. A shared storage
/// appears once (the answer has it once per node); one that is local to each
/// node of a cluster gets its node in the label.
fn storage_bars(doc: &Json) -> Option<Block> {
    let items: Vec<&Json> = entries(doc)?.collect();
    let of_type = |kind: &'static str| {
        items
            .iter()
            .copied()
            .filter(move |item| text(item, "type") == Some(kind))
    };
    let nodes = of_type("node").count();
    let mut seen = std::collections::BTreeSet::new();
    let mut bars: Vec<Bar> = of_type("storage")
        .filter(|s| text(s, "status").is_none_or(|status| status == "available"))
        .filter_map(|storage| {
            let name = api::clean_text(text(storage, "storage")?, LINE_NAME_CHARS);
            if name.is_empty() {
                return None;
            }
            let shared = amount(storage.get("shared")) == Some(1.0)
                || storage.get("shared") == Some(&Json::Bool(true));
            let node = text(storage, "node")
                .map(|n| api::clean_text(n, LINE_NAME_CHARS))
                .unwrap_or_default();
            let place = if shared { String::new() } else { node.clone() };
            if seen.contains(&(name.clone(), place.clone())) {
                return None;
            }
            // Each part is cut so that the node still shows in the 80
            // characters of a line.
            let part = |text: &str| text.chars().take(LABEL_PART_CHARS).collect::<String>();
            let label = if shared || nodes < 2 || node.is_empty() {
                name.clone()
            } else {
                format!("{} · {}", part(&name), part(&node))
            };
            // Only a storage that gives a bar counts as seen: a shared one
            // that is unreadable on one node may be readable on the next.
            let bar = disk_fill(byte_bar(label, storage, "disk", "maxdisk")?);
            seen.insert((name, place));
            Some(bar)
        })
        .collect();
    bars.sort_by(|a, b| b.pct.total_cmp(&a.pct));
    (!bars.is_empty()).then(|| Block::Bars {
        title_key: "pve_storage".into(),
        bars,
    })
}

/// The detail blocks of a `cluster/resources` answer: the nodes' bars, the
/// running and the stopped guests, the storage bars. Names go only into these
/// blocks (the poller cuts them down; see [`super::block`]).
pub fn details(doc: &Json) -> Vec<Block> {
    [
        node_blocks(doc),
        running_list(doc).into_iter().collect(),
        stopped_list(doc).into_iter().collect(),
        storage_bars(doc).into_iter().collect(),
    ]
    .into_iter()
    .flatten()
    .collect()
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
    // The card is as bad as the worst of its needs.
    let status = if reading.nodes_offline > 0.0 || storage_level == Some(Level::Bad) {
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
    answer(&doc)
}

/// The snapshot of a `cluster/resources` answer.
fn answer(doc: &Json) -> Result<Snapshot, FetchError> {
    let reading = read_answer(doc)?;
    Ok(snapshot(&reading).with_details(details(doc)))
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

    fn default_size(&self) -> crate::layout::Size {
        crate::layout::Size::Wide
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
        let credential = auth(&settings, Some("1234-abcd")).unwrap();
        let request = credential
            .apply(reqwest::Client::new().get("http://h/"))
            .build()
            .unwrap();
        let value = request.headers().get("authorization").unwrap();
        assert!(value.is_sensitive());
        assert_eq!(
            value.to_str().unwrap(),
            "PVEAPIToken=root@pam!magpie=1234-abcd"
        );
        assert!(!format!("{credential:?}").contains("1234-abcd"));
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
        for doc in [json!({}), json!({ "data": null }), json!({ "data": {} })] {
            assert_eq!(read_answer(&doc).unwrap_err(), FetchError::Invalid);
        }
        // A list with no node is what a token that sees nothing gets.
        for doc in [
            json!({ "data": [] }),
            json!({ "data": [{ "type": "qemu", "status": "running" }] }),
        ] {
            assert_eq!(read_answer(&doc).unwrap_err(), FetchError::NothingVisible);
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
        assert_eq!(s.status, Status::Bad, "as bad as the worst need");
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

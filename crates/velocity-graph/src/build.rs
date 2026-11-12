//! Building the records: subsystems, files and symbols, and their addresses.

use std::collections::HashMap;

use crate::graph::{Edge, FileInput, Provenance, Record, RecordKind, Relation};
use crate::subsystems::{Subsystem, depends, owner};
use sha2::{Digest, Sha256};

pub struct Builder {
    subsystems: Vec<Subsystem>,
    records: Vec<Record>,
    /// Address keys, one per record: what the address is hashed from.
    keys: Vec<String>,
    /// Subsystem folder to its record.
    subsystem_records: HashMap<String, usize>,
}

impl Builder {
    pub fn new(subsystems: Vec<Subsystem>) -> Self {
        Self {
            subsystems,
            records: Vec::new(),
            keys: Vec::new(),
            subsystem_records: HashMap::new(),
        }
    }

    fn push(&mut self, record: Record, key: String) -> usize {
        self.records.push(record);
        self.keys.push(key);
        self.records.len() - 1
    }

    fn subsystem_record(&mut self, path: &str) -> usize {
        let (folder, package) = owner(&self.subsystems, path);
        if let Some(index) = self.subsystem_records.get(&folder) {
            return *index;
        }
        let name = package.map_or_else(
            || {
                if folder.is_empty() {
                    "(root)".to_owned()
                } else {
                    folder.clone()
                }
            },
            |index| self.subsystems[index].name.clone(),
        );
        let record = blank(RecordKind::Subsystem, &name, &folder);
        let index = self.push(record, format!("3|{folder}"));
        self.records[index].subsystem = index;
        self.subsystem_records.insert(folder, index);
        index
    }

    pub fn add_file(&mut self, file: &FileInput) {
        let subsystem = self.subsystem_record(&file.path);
        let name = file.path.rsplit('/').next().unwrap_or(&file.path);
        let mut record = blank(RecordKind::File, name, &file.path);
        record.up = Some(subsystem);
        record.subsystem = subsystem;
        record.span_hash = file.hash.clone();
        let file_index = self.push(record, format!("2|{}", file.path));
        self.records[subsystem].down.push(file_index);
        let mut ordinals: HashMap<String, u32> = HashMap::new();
        for definition in &file.parse.definitions {
            let qualified = definition.container.as_ref().map_or_else(
                || definition.name.clone(),
                |container| format!("{container}::{}", definition.name),
            );
            let base = format!("1|{}|{qualified}|{:?}", file.path, definition.kind);
            let ordinal = ordinals.entry(base.clone()).or_insert(0);
            let key = format!("{base}|{ordinal}");
            *ordinal += 1;
            let record = Record {
                address: String::new(),
                kind: RecordKind::Symbol(definition.kind),
                name: definition.name.clone(),
                qualified,
                path: file.path.clone(),
                start_line: definition.start_line,
                end_line: definition.end_line,
                start_byte: definition.start_byte,
                end_byte: definition.end_byte,
                signature: definition.signature.clone(),
                doc: definition.doc.clone(),
                span_hash: definition.span_hash.clone(),
                up: Some(file_index),
                down: Vec::new(),
                subsystem,
                container: definition.container.clone(),
                trait_name: definition.trait_name.clone(),
                command: definition.command,
            };
            let symbol = self.push(record, key);
            // `down` of a file lines up with its parse's definitions; resolution relies on it.
            self.records[file_index].down.push(symbol);
        }
    }

    /// `depends` edges between subsystems, read from their manifests.
    pub fn dependencies(&self, manifests: &HashMap<String, String>) -> Vec<Edge> {
        let mut edges = Vec::new();
        for subsystem in &self.subsystems {
            let (Some(manifest), Some(from)) = (
                subsystem
                    .manifest
                    .as_ref()
                    .and_then(|path| manifests.get(path)),
                self.subsystem_records.get(&subsystem.folder),
            ) else {
                continue;
            };
            for other in &self.subsystems {
                let Some(to) = self.subsystem_records.get(&other.folder) else {
                    continue;
                };
                if other.folder != subsystem.folder && depends(manifest, &other.name) {
                    edges.push(Edge {
                        from: *from,
                        to: *to,
                        relation: Relation::Depends,
                        provenance: Provenance::Extracted,
                        line: 0,
                    });
                }
            }
        }
        edges
    }

    /// The records with their addresses, and the subsystems.
    pub fn finish(mut self) -> (Vec<Record>, Vec<Subsystem>) {
        assign_addresses(&mut self.records, &self.keys);
        (self.records, self.subsystems)
    }
}

fn blank(kind: RecordKind, name: &str, path: &str) -> Record {
    Record {
        address: String::new(),
        kind,
        name: name.to_owned(),
        qualified: name.to_owned(),
        path: path.to_owned(),
        start_line: 0,
        end_line: 0,
        start_byte: 0,
        end_byte: 0,
        signature: String::new(),
        doc: String::new(),
        span_hash: String::new(),
        up: None,
        down: Vec::new(),
        subsystem: 0,
        container: None,
        trait_name: None,
        command: false,
    }
}

/// `<plane>.<base-36>`: five characters of a hash of the record's key, more only on a
/// collision. Keys are taken in sorted order so a collision always lengthens the same one.
fn assign_addresses(records: &mut [Record], keys: &[String]) {
    let mut order: Vec<usize> = (0..records.len()).collect();
    order.sort_by(|left, right| keys[*left].cmp(&keys[*right]));
    let mut taken = std::collections::HashSet::new();
    for index in order {
        let code = address_code(&keys[index]);
        let plane = records[index].kind.plane();
        let address = (5..=code.len())
            .map(|length| format!("{plane}.{}", &code[..length]))
            .find(|address| !taken.contains(address))
            .unwrap_or_else(|| format!("{plane}.{code}{index}"));
        taken.insert(address.clone());
        records[index].address = address;
    }
}

/// Twelve base-36 characters from a hash of `key`.
fn address_code(key: &str) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let hash = Sha256::digest(key.as_bytes());
    let mut value = u64::from_le_bytes([
        hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
    ]);
    let mut out = String::with_capacity(12);
    for _ in 0..12 {
        out.push(ALPHABET[(value % 36) as usize] as char);
        value /= 36;
    }
    out
}

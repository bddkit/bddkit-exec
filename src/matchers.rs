//! What the assertions compare with. The JSON semantics are the host's own
//! (docs: `contains` is loose, `equals` is strict), re-implemented because a
//! plugin shares no code with the host.

use serde_json::Value;

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 200 {
        format!("{}…", s.chars().take(200).collect::<String>())
    } else {
        s
    }
}

/// `contains` = object subset, order-independent array containment, extras
/// allowed. `equals` = exact keys, element by element, same length.
pub fn json_cmp(actual: &Value, expected: &Value, path: &str, strict: bool) -> Result<(), String> {
    let mismatch = |expected: String, actual: String| {
        Err(format!("{path}: expected {expected}, got {actual}"))
    };
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            if strict && a.len() != e.len() {
                return mismatch(
                    format!("an object with {} fields", e.len()),
                    format!("{} fields", a.len()),
                );
            }
            for (key, ev) in e {
                let child = format!("{path}.{key}");
                let Some(av) = a.get(key) else {
                    return Err(format!(
                        "{child}: expected {}, the field is missing",
                        short(ev)
                    ));
                };
                json_cmp(av, ev, &child, strict)?;
            }
            Ok(())
        }
        (Value::Array(a), Value::Array(e)) if strict => {
            if a.len() != e.len() {
                return mismatch(
                    format!("an array of length {}", e.len()),
                    format!("length {}", a.len()),
                );
            }
            a.iter()
                .zip(e)
                .enumerate()
                .try_for_each(|(i, (av, ev))| json_cmp(av, ev, &format!("{path}[{i}]"), strict))
        }
        (Value::Array(a), Value::Array(e)) => {
            for ev in e {
                if !a.iter().any(|av| json_cmp(av, ev, "", false).is_ok()) {
                    return mismatch(format!("an array containing {}", short(ev)), short(actual));
                }
            }
            Ok(())
        }
        _ if actual == expected => Ok(()),
        _ => mismatch(short(expected), short(actual)),
    }
}

/// Reads `a.b[0].c`, with an optional `root.` prefix — the host's path syntax.
pub fn json_read<'a>(root: &'a Value, path: &str) -> Result<&'a Value, String> {
    let path = path.trim();
    let path = path
        .strip_prefix("root.")
        .unwrap_or(if path == "root" { "" } else { path });
    let mut cur = root;
    let mut walked = String::from("root");
    for segment in path.split('.').filter(|s| !s.is_empty()) {
        let (name, mut rest) = segment.split_at(segment.find('[').unwrap_or(segment.len()));
        if !name.is_empty() {
            walked.push('.');
            walked.push_str(name);
            cur = cur
                .get(name)
                .ok_or_else(|| format!("path {walked} does not exist"))?;
        }
        while !rest.is_empty() {
            let close = rest
                .find(']')
                .ok_or_else(|| format!("unclosed bracket in {segment:?}"))?;
            let index: usize = rest[1..close]
                .parse()
                .map_err(|_| format!("index must be a number in {segment:?}"))?;
            walked.push_str(&format!("[{index}]"));
            cur = cur
                .get(index)
                .ok_or_else(|| format!("path {walked} does not exist"))?;
            rest = &rest[close + 1..];
            if !rest.is_empty() && !rest.starts_with('[') {
                return Err(format!("unexpected text after an index in {segment:?}"));
            }
        }
    }
    Ok(cur)
}

/// A JSON value as a variable: a string without its quotes, anything else in
/// its JSON spelling — the host's rule for `extract … from JSON`.
pub fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Delimited {
    Csv,
    Tsv,
}

impl Delimited {
    pub fn parse(word: &str) -> Self {
        if word == "TSV" { Self::Tsv } else { Self::Csv }
    }

    /// Rows of cells. CSV is RFC 4180: `"` quotes a cell, `""` inside one is
    /// a literal quote, and a quoted cell may span lines. TSV has no quoting
    /// at all — a tab separates, a newline ends the row. `\r\n` is accepted
    /// for either. A trailing line ending adds no row.
    pub fn rows(self, text: &str) -> Result<Vec<Vec<String>>, String> {
        match self {
            Self::Tsv => Ok(text
                .lines()
                .map(|line| line.split('\t').map(str::to_string).collect())
                .collect()),
            Self::Csv => csv_rows(text),
        }
    }
}

fn csv_rows(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut chars = text.chars().peekable();
    let mut line = 1;
    // Whether the current row has anything in it yet, so a trailing newline
    // does not produce an empty row.
    let mut started = false;
    while let Some(c) = chars.next() {
        match c {
            '"' if cell.is_empty() => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') if chars.peek() == Some(&'"') => {
                            chars.next();
                            cell.push('"');
                        }
                        Some('"') => break,
                        Some(c) => {
                            if c == '\n' {
                                line += 1;
                            }
                            cell.push(c);
                        }
                        None => {
                            return Err(format!("CSV line {line}: a quoted cell is never closed"));
                        }
                    }
                }
                if !matches!(chars.peek(), None | Some(',' | '\n' | '\r')) {
                    return Err(format!("CSV line {line}: text after a closing quote"));
                }
            }
            ',' => {
                started = true;
                row.push(std::mem::take(&mut cell));
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                line += 1;
                if started {
                    row.push(std::mem::take(&mut cell));
                    rows.push(std::mem::take(&mut row));
                }
                started = false;
            }
            c => {
                started = true;
                cell.push(c);
            }
        }
    }
    if started {
        row.push(cell);
        rows.push(row);
    }
    Ok(rows)
}

/// `contains`: the table's header names columns the stream's header must
/// have (any order, extras allowed), and each table row must appear in some
/// stream row on those columns. `equals`: the same header, and the same rows
/// in the same order.
pub fn table_cmp(
    stream: &[Vec<String>],
    table: &[Vec<String>],
    strict: bool,
) -> Result<(), String> {
    let (Some(header), Some(want_header)) = (stream.first(), table.first()) else {
        return Err("the output has no header line".into());
    };
    let (records, wanted) = (&stream[1..], &table[1..]);
    if let Some(i) = records.iter().position(|r| r.len() != header.len()) {
        return Err(format!(
            "row {} has {} cells, the header has {}",
            i + 1,
            records[i].len(),
            header.len()
        ));
    }
    if strict {
        if header != want_header {
            return Err(format!(
                "expected the columns {want_header:?}, got {header:?}"
            ));
        }
        if records.len() != wanted.len() {
            return Err(format!(
                "expected {} rows, got {}",
                wanted.len(),
                records.len()
            ));
        }
        return match records.iter().zip(wanted).position(|(r, w)| r != w) {
            Some(i) => Err(format!(
                "row {}: expected {:?}, got {:?}",
                i + 1,
                wanted[i],
                records[i]
            )),
            None => Ok(()),
        };
    }
    let columns: Vec<usize> = want_header
        .iter()
        .map(|name| {
            header
                .iter()
                .position(|h| h == name)
                .ok_or_else(|| format!("no column {name:?} in the header {header:?}"))
        })
        .collect::<Result<_, _>>()?;
    for want in wanted {
        if !records
            .iter()
            .any(|r| columns.iter().zip(want).all(|(&c, w)| &r[c] == w))
        {
            let shown: Vec<String> = want_header
                .iter()
                .zip(want)
                .map(|(h, w)| format!("{h}={w}"))
                .collect();
            return Err(format!("no row with {}", shown.join(", ")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rows(t: &[&[&str]]) -> Vec<Vec<String>> {
        t.iter()
            .map(|r| r.iter().map(|c| c.to_string()).collect())
            .collect()
    }

    #[test]
    fn contains_json_is_a_subset_with_unordered_arrays() {
        let a = json!({"id": 1, "tags": ["a", "b", "c"], "extra": true});
        assert!(json_cmp(&a, &json!({"tags": ["c", "a"]}), "root", false).is_ok());
        let e = json_cmp(&a, &json!({"tags": ["z"]}), "root", false).unwrap_err();
        assert!(e.starts_with("root.tags"), "{e}");
    }

    #[test]
    fn equals_json_is_exact() {
        let a = json!({"id": 1, "tags": ["a", "b"]});
        assert!(json_cmp(&a, &json!({"id": 1, "tags": ["a", "b"]}), "root", true).is_ok());
        assert!(json_cmp(&a, &json!({"id": 1}), "root", true).is_err());
        assert!(json_cmp(&a, &json!({"id": 1, "tags": ["b", "a"]}), "root", true).is_err());
    }

    #[test]
    fn a_path_reads_nested_fields_and_indices() {
        let v = json!({"data": {"items": [{"id": 7}]}});
        assert_eq!(json_read(&v, "data.items[0].id"), Ok(&json!(7)));
        assert_eq!(json_read(&v, "root.data.items[0].id"), Ok(&json!(7)));
        assert_eq!(
            json_read(&v, "data.nope").unwrap_err(),
            "path root.data.nope does not exist"
        );
    }

    #[test]
    fn csv_follows_rfc_4180_quoting() {
        let parsed = Delimited::Csv
            .rows("name,note\r\n\"Smith, J\",\"say \"\"hi\"\"\"\nx,\"two\nlines\"\n")
            .expect("csv");
        assert_eq!(
            parsed,
            rows(&[
                &["name", "note"],
                &["Smith, J", "say \"hi\""],
                &["x", "two\nlines"]
            ])
        );
    }

    #[test]
    fn csv_keeps_empty_cells_and_refuses_an_open_quote() {
        assert_eq!(
            Delimited::Csv.rows("a,,b\n").expect("csv"),
            rows(&[&["a", "", "b"]])
        );
        assert!(Delimited::Csv.rows("a,\"b\n").is_err());
    }

    #[test]
    fn tsv_does_not_quote() {
        assert_eq!(
            Delimited::Tsv.rows("a\t\"b\"\n1\t2\n").expect("tsv"),
            rows(&[&["a", "\"b\""], &["1", "2"]])
        );
    }

    #[test]
    fn table_contains_matches_a_subset_of_columns_and_rows() {
        let stream = rows(&[
            &["id", "name", "role"],
            &["1", "ann", "admin"],
            &["2", "bob", "user"],
        ]);
        assert!(
            table_cmp(
                &stream,
                &rows(&[&["role", "name"], &["user", "bob"]]),
                false
            )
            .is_ok()
        );
        let e = table_cmp(&stream, &rows(&[&["name"], &["eve"]]), false).unwrap_err();
        assert_eq!(e, "no row with name=eve");
        assert!(
            table_cmp(&stream, &rows(&[&["email"], &["x"]]), false)
                .unwrap_err()
                .contains("\"email\"")
        );
    }

    #[test]
    fn table_equals_needs_the_same_columns_and_rows_in_order() {
        let stream = rows(&[&["id", "name"], &["1", "ann"], &["2", "bob"]]);
        assert!(table_cmp(&stream, &stream, true).is_ok());
        assert!(
            table_cmp(
                &stream,
                &rows(&[&["id", "name"], &["2", "bob"], &["1", "ann"]]),
                true
            )
            .is_err()
        );
        assert!(
            table_cmp(
                &stream,
                &rows(&[&["name", "id"], &["ann", "1"], &["bob", "2"]]),
                true
            )
            .is_err()
        );
    }

    #[test]
    fn a_ragged_stream_row_is_reported() {
        let stream = rows(&[&["id", "name"], &["1"]]);
        assert_eq!(
            table_cmp(&stream, &rows(&[&["id"], &["1"]]), false).unwrap_err(),
            "row 1 has 1 cells, the header has 2"
        );
    }
}

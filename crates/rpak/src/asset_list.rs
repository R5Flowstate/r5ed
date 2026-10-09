//! The `--list --listformat csv` output of the RSX CLI.
//!
//! The header is fixed: `type,pakver,rsxver,guid,file_name,asset_name`.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const HEADER: &str = "type,pakver,rsxver,guid,file_name,asset_name";

/// One row of the asset list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRow {
    /// The 4CC type code, which is also the folder name an export lands in.
    pub kind: String,
    pub pak_version: u32,
    pub rsx_version: u32,
    pub guid: u64,
    /// The pak the asset was read out of, e.g. `common.rpak`.
    pub pak: String,
    /// Asset path with the engine's backslashes normalised to forward slashes.
    pub path: String,
}

impl AssetRow {
    /// The file name without directory, e.g. `w_kunai_wraith.rmdl`.
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// The asset path with the extension dropped, e.g. `mdl/props/crate/crate`.
    pub fn stem(&self) -> &str {
        let name = self.file_name();
        match name.rfind('.') {
            Some(i) => {
                let cut = self.path.len() - name.len() + i;
                &self.path[..cut]
            }
            None => &self.path,
        }
    }
}

/// Splits one CSV record, honouring `"..."` quoting with `""` as an escaped quote.
fn split_record(line: &str) -> Result<Vec<String>> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut field = String::new();
    let mut i = 0;
    let mut quoted = false;
    while i < b.len() {
        let c = b[i] as char;
        if quoted {
            if c == '"' {
                if b.get(i + 1) == Some(&b'"') {
                    field.push('"');
                    i += 2;
                    continue;
                }
                quoted = false;
                i += 1;
                continue;
            }
            let n = line[i..].chars().next().map_or(1, char::len_utf8);
            field.push_str(&line[i..i + n]);
            i += n;
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                i += 1;
            }
            ',' => {
                out.push(std::mem::take(&mut field));
                i += 1;
            }
            '\r' => i += 1,
            _ => {
                let n = line[i..].chars().next().map_or(1, char::len_utf8);
                field.push_str(&line[i..i + n]);
                i += n;
            }
        }
    }
    if quoted {
        return Err(Error::CsvQuote(i));
    }
    out.push(field);
    Ok(out)
}

fn parse_u32(row: usize, column: &'static str, s: &str) -> Result<u32> {
    s.trim().parse::<u32>().map_err(|_| Error::CsvNumber { row, column, value: s.to_string() })
}

/// `rsxver` is written as `v10`, the version with the tool's own prefix.
fn parse_versioned(row: usize, s: &str) -> Result<u32> {
    let t = s.trim();
    let body = t.strip_prefix('v').or_else(|| t.strip_prefix('V')).unwrap_or(t);
    body.parse::<u32>().map_err(|_| Error::CsvNumber { row, column: "rsxver", value: s.to_string() })
}

fn parse_guid(row: usize, column: &'static str, s: &str) -> Result<u64> {
    let t = s.trim();
    let body = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    u64::from_str_radix(body, 16)
        .map_err(|_| Error::CsvGuid { row, column, value: s.to_string() })
}

/// Parses the whole CSV, header included. Blank lines are skipped.
pub fn parse_asset_list(text: &str) -> Result<Vec<AssetRow>> {
    let mut lines = text.lines();
    let header = lines.next().ok_or(Error::CsvHeader {
        expected: HEADER.to_string(),
        found: String::new(),
    })?;
    let header = header.trim_end_matches('\r');
    if header != HEADER {
        return Err(Error::CsvHeader { expected: HEADER.to_string(), found: header.to_string() });
    }
    let mut rows = Vec::new();
    // The header is line 1, so the first data row is line 2.
    for (i, line) in lines.enumerate() {
        let row = i + 2;
        if line.trim().is_empty() {
            continue;
        }
        let cols = split_record(line)?;
        if cols.len() != 6 {
            return Err(Error::CsvArity { row, expected: 6, found: cols.len() });
        }
        rows.push(AssetRow {
            kind: cols[0].trim().to_string(),
            pak_version: parse_u32(row, "pakver", &cols[1])?,
            rsx_version: parse_versioned(row, &cols[2])?,
            guid: parse_guid(row, "guid", &cols[3])?,

            pak: cols[4].trim().to_string(),
            path: cols[5].replace('\\', "/"),
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        "type,pakver,rsxver,guid,file_name,asset_name\r\n",
        "txtr,10,v10,a6132a58ee9c44e8,common.rpak,texture\\0xA6132A58EE9C44E8\r\n",
        "mdl_,17,v17,bdece0eaeb560e40,common.rpak,mdl\\props\\caustic\\caustic_mug_v.rmdl\r\n",
        "matl,23,v23,3e8c2486aa4ddc64,common.rpak,material\\art\\shop\\a.rpak\r\n",
    );

    #[test]
    fn rsx_csv_parse() {
        let rows = parse_asset_list(SAMPLE).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].kind, "txtr");
        assert_eq!(rows[0].pak_version, 10);
        assert_eq!(rows[0].rsx_version, 10);
        assert_eq!(rows[0].guid, 0xa613_2a58_ee9c_44e8);
        assert_eq!(rows[0].pak, "common.rpak");
        assert_eq!(rows[0].path, "texture/0xA6132A58EE9C44E8");
        assert_eq!(rows[1].kind, "mdl_");
        assert_eq!(rows[1].rsx_version, 17);
    }

    #[test]
    fn normalises_backslashes_and_derives_stems() {
        let rows = parse_asset_list(SAMPLE).unwrap();
        assert_eq!(rows[1].path, "mdl/props/caustic/caustic_mug_v.rmdl");
        assert_eq!(rows[1].file_name(), "caustic_mug_v.rmdl");
        assert_eq!(rows[1].stem(), "mdl/props/caustic/caustic_mug_v");
        assert_eq!(rows[2].stem(), "material/art/shop/a");
    }

    #[test]
    fn quoted_fields_keep_commas_and_escaped_quotes() {
        let csv = "type,pakver,rsxver,guid,file_name,asset_name\n\
                   wrap,1,v1,1,common.rpak,\"maps/a,b.bsp\"\n\
                   wrap,1,v1,2,common.rpak,\"q\"\"z\"\n";
        let rows = parse_asset_list(csv).unwrap();
        assert_eq!(rows[0].path, "maps/a,b.bsp");
        assert_eq!(rows[1].path, "q\"z");
    }

    #[test]
    fn skips_blank_lines() {
        let csv = "type,pakver,rsxver,guid,file_name,asset_name\n\ntxtr,1,v1,ff,a.rpak,x\\y\n\n";
        assert_eq!(parse_asset_list(csv).unwrap().len(), 1);
    }

    #[test]
    fn rejects_a_wrong_header() {
        let csv = "type,pakver,guid\ntxtr,1,2\n";
        assert!(matches!(parse_asset_list(csv), Err(Error::CsvHeader { .. })));
    }

    #[test]
    fn rejects_short_rows_and_bad_numbers() {
        let h = "type,pakver,rsxver,guid,file_name,asset_name\n";
        assert!(matches!(parse_asset_list(&format!("{h}txtr,1\n")), Err(Error::CsvArity { row: 2, expected: 6, found: 2 })));
        assert!(matches!(
            parse_asset_list(&format!("{h}txtr,x,v1,ff,a,b\n")),
            Err(Error::CsvNumber { row: 2, column: "pakver", .. })
        ));
        assert!(matches!(
            parse_asset_list(&format!("{h}txtr,1,v1,zz,a,b\n")),
            Err(Error::CsvGuid { row: 2, column: "guid", .. })
        ));
    }

    #[test]
    fn rejects_an_unterminated_quote() {
        let h = "type,pakver,rsxver,guid,file_name,asset_name\n";
        assert!(matches!(parse_asset_list(&format!("{h}a,b,c,d,e,\"f\n")), Err(Error::CsvQuote(_))));
    }

    #[test]
    fn accepts_a_0x_prefixed_guid_and_rejects_empty_input() {
        let h = "type,pakver,rsxver,guid,file_name,asset_name\n";
        assert_eq!(parse_asset_list(&format!("{h}txtr,1,v1,0xFF,a,b\n")).unwrap()[0].guid, 255);
        assert!(matches!(parse_asset_list(""), Err(Error::CsvHeader { .. })));
    }
}

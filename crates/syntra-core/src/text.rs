//! 워크스페이스 파일의 인코딩과 줄바꿈을 다룬다. 승인은 내용만 바꾸고 파일 형식은 원본 그대로 둔다.

use encoding_rs::EUC_KR;
use serde::Deserialize;

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";
const UTF16LE_BOM: &[u8] = b"\xFF\xFE";
/// CP949로 해석되지 않는 바이트(중국어 GBK 주석, 깨진 문자 등)를 담아 두는 문자 영역.
/// 보조 사용자 정의 영역 B의 끝이라 CP949를 읽어서는 나올 수 없다.
const RAW_BYTE_BASE: u32 = 0x10_FF00;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum Encoding {
    #[default]
    #[serde(rename = "utf-8")]
    Utf8,
    #[serde(rename = "utf-8-bom")]
    Utf8Bom,
    /// Visual Studio가 "유니코드"로 저장한 파일.
    #[serde(rename = "utf-16le-bom")]
    Utf16LeBom,
    /// 한국어 Windows 코드 페이지. encoding_rs의 EUC-KR은 UHC 확장까지 포함한 CP949와 같다.
    #[serde(rename = "cp949")]
    Cp949,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineEnding {
    Lf,
    Crlf,
}

/// 워크스페이스의 `.syntra/config.json`. 파일이 없으면 기본값을 쓴다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// 새 파일과 ASCII만 있던 파일에 한글 등을 넣을 때 쓰는 인코딩.
    #[serde(default)]
    pub encoding: Encoding,
    /// 새 파일과 줄바꿈이 없던 파일에 쓰는 줄바꿈. 없으면 제안서의 줄바꿈을 그대로 쓴다.
    #[serde(default)]
    pub line_ending: Option<LineEnding>,
}

/// 디스크에서 읽은 파일. 저장할 때 읽었던 인코딩으로 되돌린다.
#[derive(Debug)]
pub(crate) struct SourceText {
    pub text: String,
    /// ASCII만 있는 파일은 인코딩을 알 수 없으므로 None으로 두고 설정을 따른다.
    encoding: Option<Encoding>,
}

impl SourceText {
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if let Some(rest) = bytes.strip_prefix(UTF8_BOM) {
            let text = std::str::from_utf8(rest)
                .map_err(|_| "UTF-8 BOM이 있지만 UTF-8이 아닌 내용이 섞여 있습니다".to_string())?;
            return Ok(Self::new(text.to_string(), Some(Encoding::Utf8Bom)));
        }
        if let Some(rest) = bytes.strip_prefix(UTF16LE_BOM) {
            let units: Vec<u16> = rest
                .chunks(2)
                .map(|pair| match pair {
                    [low, high] => Ok(u16::from_le_bytes([*low, *high])),
                    _ => Err("UTF-16 파일의 길이가 홀수입니다".to_string()),
                })
                .collect::<Result<_, _>>()?;
            let text = String::from_utf16(&units).map_err(|_| {
                "UTF-16 BOM이 있지만 올바르지 않은 문자가 섞여 있습니다".to_string()
            })?;
            return Ok(Self::new(text, Some(Encoding::Utf16LeBom)));
        }
        if let Ok(text) = std::str::from_utf8(bytes) {
            let encoding = (!bytes.is_ascii()).then_some(Encoding::Utf8);
            return Ok(Self::new(text.to_string(), encoding));
        }
        let text = decode_cp949(bytes);
        // 다시 인코딩했을 때 원본과 다르면 편집하지 않은 부분까지 바뀌므로 거부한다.
        if encode(&text, Encoding::Cp949).ok().as_deref() != Some(bytes) {
            return Err("CP949로 읽은 내용을 원본과 똑같이 되돌릴 수 없습니다".to_string());
        }
        Ok(Self::new(text, Some(Encoding::Cp949)))
    }

    fn new(text: String, encoding: Option<Encoding>) -> Self {
        Self { text, encoding }
    }

    pub fn encoding_name(&self) -> &'static str {
        match self.encoding {
            None => "ASCII",
            Some(Encoding::Utf8) => "UTF-8",
            Some(Encoding::Utf8Bom) => "UTF-8 BOM",
            Some(Encoding::Utf16LeBom) => "UTF-16 LE BOM",
            Some(Encoding::Cp949) if self.text.contains(is_raw_byte) => {
                "CP949 (해석되지 않는 바이트 포함)"
            }
            Some(Encoding::Cp949) => "CP949",
        }
    }

    /// 바뀐 내용을 원본 인코딩으로 되돌린다. ASCII 파일은 ASCII로 남으면 그대로 쓴다.
    pub fn encode(&self, text: &str, fallback: Encoding) -> Result<Vec<u8>, String> {
        match self.encoding {
            Some(encoding) => encode(text, encoding),
            None if text.is_ascii() => Ok(text.as_bytes().to_vec()),
            None => encode(text, fallback),
        }
    }
}

pub(crate) fn encode(text: &str, encoding: Encoding) -> Result<Vec<u8>, String> {
    match encoding {
        Encoding::Utf8 => Ok(text.as_bytes().to_vec()),
        Encoding::Utf8Bom => Ok([UTF8_BOM, text.as_bytes()].concat()),
        Encoding::Utf16LeBom => Ok(UTF16LE_BOM
            .iter()
            .copied()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect()),
        Encoding::Cp949 => {
            let mut bytes = Vec::with_capacity(text.len());
            let mut rest = text;
            while let Some(pos) = rest.find(is_raw_byte) {
                let raw = rest[pos..].chars().next().unwrap_or_default();
                bytes.extend_from_slice(&encode_cp949(&rest[..pos])?);
                bytes.push((raw as u32 - RAW_BYTE_BASE) as u8);
                rest = &rest[pos + raw.len_utf8()..];
            }
            bytes.extend_from_slice(&encode_cp949(rest)?);
            Ok(bytes)
        }
    }
}

/// CP949로 읽는다. 해석되지 않는 바이트는 버리지 않고 RAW_BYTE_BASE 영역의 문자로 남겨
/// 저장할 때 원래 바이트로 되돌린다. Visual C++도 이런 바이트를 그대로 컴파일한다.
fn decode_cp949(bytes: &[u8]) -> String {
    if let Some(text) = EUC_KR.decode_without_bom_handling_and_without_replacement(bytes) {
        return text.into_owned();
    }
    let mut text = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte.is_ascii() {
            text.push(byte as char);
            i += 1;
            continue;
        }
        if let Some(pair) = bytes.get(i..i + 2)
            && let Some(decoded) = EUC_KR.decode_without_bom_handling_and_without_replacement(pair)
        {
            text.push_str(&decoded);
            i += 2;
            continue;
        }
        text.push(char::from_u32(RAW_BYTE_BASE + u32::from(byte)).unwrap_or_default());
        i += 1;
    }
    text
}

/// 화면에 보여줄 때는 해석되지 않는 바이트를 U+FFFD로 바꾼다.
/// 이 문자가 든 줄은 `before`에 옮겨 적어도 파일과 일치하지 않는다.
pub(crate) fn for_display(text: &str) -> String {
    text.chars()
        .map(|c| if is_raw_byte(c) { '\u{FFFD}' } else { c })
        .collect()
}

fn is_raw_byte(c: char) -> bool {
    (RAW_BYTE_BASE + 0x80..=RAW_BYTE_BASE + 0xFF).contains(&(c as u32))
}

fn encode_cp949(text: &str) -> Result<Vec<u8>, String> {
    let (bytes, _, had_errors) = EUC_KR.encode(text);
    if had_errors {
        let mut buf = [0; 4];
        let unmappable = text
            .chars()
            .find(|c| EUC_KR.encode(c.encode_utf8(&mut buf)).2)
            .unwrap_or_default();
        return Err(format!(
            "CP949로 표현할 수 없는 문자가 있습니다: '{unmappable}' (U+{:04X})",
            unmappable as u32
        ));
    }
    Ok(bytes.into_owned())
}

impl LineEnding {
    /// 텍스트의 줄바꿈을 모두 이 줄바꿈으로 바꾼다.
    pub fn apply(self, text: &str) -> String {
        let lf = text.replace("\r\n", "\n");
        match self {
            LineEnding::Lf => lf,
            LineEnding::Crlf => lf.replace('\n', "\r\n"),
        }
    }
}

/// 파일이 쓰는 줄바꿈.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileLineEnding {
    Uniform(LineEnding),
    /// 줄마다 다르면 어느 쪽으로도 맞출 수 없으므로 제안서를 그대로 비교한다.
    Mixed,
    Absent,
}

impl FileLineEnding {
    pub fn detect(text: &str) -> Self {
        let crlf = text.matches("\r\n").count();
        let lf = text.matches('\n').count() - crlf;
        let cr = text.matches('\r').count() - crlf;
        match (crlf, lf, cr) {
            (0, 0, 0) => Self::Absent,
            (_, 0, 0) => Self::Uniform(LineEnding::Crlf),
            (0, _, 0) => Self::Uniform(LineEnding::Lf),
            _ => Self::Mixed,
        }
    }

    /// 제안서의 줄바꿈을 맞춰야 할 대상. None이면 제안서를 그대로 쓴다.
    pub fn target(self, config: &Config) -> Option<LineEnding> {
        match self {
            Self::Uniform(line_ending) => Some(line_ending),
            Self::Mixed => None,
            Self::Absent => config.line_ending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "한글" 과 UHC 확장 영역 문자 "똠"(0x8C63)을 CP949로 인코딩한 값.
    const CP949: &[u8] = b"// \xC7\xD1\xB1\xDB \x8C\x63\r\n";

    #[test]
    fn cp949_round_trips_including_uhc_extension() {
        let source = SourceText::decode(CP949).unwrap();
        assert_eq!(source.text, "// 한글 똠\r\n");
        assert_eq!(source.encode(&source.text, Encoding::Utf8).unwrap(), CP949);
    }

    #[test]
    fn undecodable_bytes_are_kept_as_they_were() {
        // GBK 중국어 주석("新网")과 줄 끝의 끊긴 리드 바이트. CP949로는 해석되지 않는다.
        let bytes = b"// \xD0\xC2\xCD\xF8 \xC9\xCF\r\nint a; // \xB6\r\n";
        let source = SourceText::decode(bytes).unwrap();
        assert!(source.text.ends_with("int a; // \u{10FFB6}\r\n"));
        assert_eq!(source.encode(&source.text, Encoding::Utf8).unwrap(), bytes);
        let edited = source.text.replace("int a;", "int b; // 한글");
        assert_eq!(
            source.encode(&edited, Encoding::Utf8).unwrap(),
            [
                &bytes[..12],
                b"int b; // \xC7\xD1\xB1\xDB // \xB6\r\n".as_slice()
            ]
            .concat()
        );
    }

    #[test]
    fn utf16_files_round_trip() {
        let bytes: Vec<u8> = b"\xFF\xFE"
            .iter()
            .copied()
            .chain("// 한글\r\n".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let source = SourceText::decode(&bytes).unwrap();
        assert_eq!(source.text, "// 한글\r\n");
        assert_eq!(source.encode(&source.text, Encoding::Cp949).unwrap(), bytes);
    }

    #[test]
    fn invalid_bytes_are_refused() {
        assert!(SourceText::decode(b"\xEF\xBB\xBF\xC7\xD1").is_err());
        assert!(SourceText::decode(b"\xFF\xFE\x41").is_err());
        assert!(SourceText::decode(b"\xFF\xFE\x00\xD8").is_err());
    }

    #[test]
    fn utf8_files_keep_their_bom() {
        let source = SourceText::decode("\u{FEFF}가".as_bytes()).unwrap();
        assert_eq!(source.text, "가");
        assert_eq!(
            source.encode("나", Encoding::Cp949).unwrap(),
            "\u{FEFF}나".as_bytes()
        );
        let source = SourceText::decode("가".as_bytes()).unwrap();
        assert_eq!(
            source.encode("나", Encoding::Cp949).unwrap(),
            "나".as_bytes()
        );
    }

    #[test]
    fn ascii_files_follow_the_configured_encoding() {
        let source = SourceText::decode(b"int a;\r\n").unwrap();
        assert_eq!(
            source.encode("int b;", Encoding::Utf8Bom).unwrap(),
            b"int b;"
        );
        assert_eq!(
            source.encode("// 한글", Encoding::Cp949).unwrap(),
            b"// \xC7\xD1\xB1\xDB"
        );
    }

    #[test]
    fn unmappable_character_is_reported() {
        let error = encode("a😀", Encoding::Cp949).unwrap_err();
        assert!(error.contains("U+1F600"), "{error}");
    }

    #[test]
    fn line_endings_are_detected() {
        use FileLineEnding::*;
        assert_eq!(
            FileLineEnding::detect("a\r\nb\r\n"),
            Uniform(LineEnding::Crlf)
        );
        assert_eq!(FileLineEnding::detect("a\nb\n"), Uniform(LineEnding::Lf));
        assert_eq!(FileLineEnding::detect("a\r\nb\n"), Mixed);
        assert_eq!(FileLineEnding::detect("a\rb"), Mixed);
        assert_eq!(FileLineEnding::detect("a"), Absent);
    }

    #[test]
    fn line_endings_are_converted() {
        assert_eq!(LineEnding::Crlf.apply("a\nb\r\n"), "a\r\nb\r\n");
        assert_eq!(LineEnding::Lf.apply("a\r\nb\n"), "a\nb\n");
    }

    /// 실제 소스 트리의 모든 C/C++ 파일이 바이트 그대로 되돌아오는지 확인한다.
    /// `SYNTRA_ROUNDTRIP_DIR=<폴더> cargo test -p syntra-core -- --ignored round_trip`
    #[test]
    #[ignore]
    fn round_trip_source_tree() {
        fn walk(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                let source = path.extension().is_some_and(|ext| {
                    ["c", "cpp", "h", "hpp", "inl"]
                        .contains(&ext.to_string_lossy().to_lowercase().as_str())
                });
                if path.is_dir() {
                    walk(&path, files);
                } else if source {
                    files.push(path);
                }
            }
        }
        let dir = std::env::var_os("SYNTRA_ROUNDTRIP_DIR").expect("SYNTRA_ROUNDTRIP_DIR");
        let mut files = Vec::new();
        walk(std::path::Path::new(&dir), &mut files);
        let failures: Vec<String> = files
            .iter()
            .filter_map(|path| {
                let bytes = std::fs::read(path).unwrap();
                let result = SourceText::decode(&bytes)
                    .and_then(|source| source.encode(&source.text, Encoding::Utf8));
                match result {
                    Ok(encoded) if encoded == bytes => None,
                    Ok(_) => Some(format!("{}: 원본과 다름", path.display())),
                    Err(e) => Some(format!("{}: {e}", path.display())),
                }
            })
            .collect();
        println!("{}개 파일 확인", files.len());
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn config_uses_lowercase_names() {
        let config: Config =
            serde_json::from_str(r#"{"encoding":"cp949","line_ending":"crlf"}"#).unwrap();
        assert_eq!(config.encoding, Encoding::Cp949);
        assert_eq!(config.line_ending, Some(LineEnding::Crlf));
        assert!(serde_json::from_str::<Config>(r#"{"encoding":"euc-kr"}"#).is_err());
    }
}

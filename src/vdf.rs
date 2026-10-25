//! Leitor de VDF/KeyValues (formato de texto da Steam: `libraryfolders.vdf`, `appmanifest_*.acf`).

#[derive(Debug, Clone, PartialEq)]
pub enum Vdf {
    Str(String),
    Obj(Vec<(String, Vdf)>),
}

impl Vdf {
    /// Filho de um objeto, sem diferenciar maiúsculas (a Steam mistura `AppID`/`appid`).
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        match self {
            Vdf::Obj(v) => v.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v),
            Vdf::Str(_) => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Vdf::Str(s) => Some(s),
            Vdf::Obj(_) => None,
        }
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key)?.str()
    }

    pub fn entries(&self) -> &[(String, Vdf)] {
        match self {
            Vdf::Obj(v) => v,
            Vdf::Str(_) => &[],
        }
    }
}

#[derive(Debug, PartialEq)]
enum Tok {
    Str(String),
    Open,
    Close,
}

fn tokenize(text: &str) -> Result<Vec<Tok>, String> {
    let mut out = vec![];
    let mut it = text.chars().peekable();
    while let Some(&c) = it.peek() {
        match c {
            c if c.is_whitespace() || c == '\u{feff}' => {
                it.next();
            }
            '/' => {
                it.next();
                if it.peek() == Some(&'/') {
                    while it.next_if(|c| *c != '\n').is_some() {}
                } else {
                    return Err("'/' solto".into());
                }
            }
            '{' => {
                it.next();
                out.push(Tok::Open);
            }
            '}' => {
                it.next();
                out.push(Tok::Close);
            }
            '"' => {
                it.next();
                let mut s = String::new();
                loop {
                    match it.next() {
                        None => return Err("texto entre aspas sem fechar".into()),
                        Some('"') => break,
                        Some('\\') => match it.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some('\\') => s.push('\\'),
                            Some('"') => s.push('"'),
                            Some(o) => {
                                s.push('\\');
                                s.push(o);
                            }
                            None => return Err("escape no fim do arquivo".into()),
                        },
                        Some(o) => s.push(o),
                    }
                }
                out.push(Tok::Str(s));
            }
            _ => {
                let mut s = String::new();
                while let Some(c) = it.next_if(|c| !c.is_whitespace() && !matches!(c, '{' | '}' | '"')) {
                    s.push(c);
                }
                out.push(Tok::Str(s));
            }
        }
    }
    Ok(out)
}

fn parse_obj(toks: &[Tok], pos: &mut usize, depth: usize) -> Result<Vec<(String, Vdf)>, String> {
    if depth > 64 {
        return Err("aninhamento profundo demais".into());
    }
    let mut v = vec![];
    while *pos < toks.len() {
        match &toks[*pos] {
            Tok::Close => return Ok(v),
            Tok::Open => return Err("'{' sem chave".into()),
            Tok::Str(k) => {
                *pos += 1;
                match toks.get(*pos) {
                    Some(Tok::Str(val)) => {
                        v.push((k.clone(), Vdf::Str(val.clone())));
                        *pos += 1;
                    }
                    Some(Tok::Open) => {
                        *pos += 1;
                        let inner = parse_obj(toks, pos, depth + 1)?;
                        if toks.get(*pos) != Some(&Tok::Close) {
                            return Err("'}' ausente".into());
                        }
                        *pos += 1;
                        v.push((k.clone(), Vdf::Obj(inner)));
                    }
                    _ => return Err(format!("chave \"{k}\" sem valor")),
                }
            }
        }
    }
    Ok(v)
}

/// Lê um documento VDF; o resultado é um objeto com as chaves de nível superior.
pub fn parse(text: &str) -> Result<Vdf, String> {
    let toks = tokenize(text)?;
    let mut pos = 0;
    let v = parse_obj(&toks, &mut pos, 0)?;
    if pos != toks.len() {
        return Err("'}' sobrando".into());
    }
    Ok(Vdf::Obj(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_objects_and_comments() {
        let v = parse("\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"/home/u/.steam\"\n\t\t// nota\n\t\t\"apps\" { \"730\" \"1\" \"440\" \"2\" }\n\t}\n}\n").unwrap();
        let lf = v.get("libraryfolders").unwrap();
        assert_eq!(lf.get("0").unwrap().get_str("path"), Some("/home/u/.steam"));
        assert_eq!(lf.get("0").unwrap().get("apps").unwrap().entries().len(), 2);
    }

    #[test]
    fn handles_escapes_bom_and_case_insensitive_keys() {
        let v = parse("\u{feff}\"AppState\" { \"AppID\" \"730\" \"name\" \"Quote \\\"x\\\" C:\\\\dir\" }").unwrap();
        let a = v.get("appstate").unwrap();
        assert_eq!(a.get_str("appid"), Some("730"));
        assert_eq!(a.get_str("name"), Some("Quote \"x\" C:\\dir"));
    }

    #[test]
    fn accepts_unquoted_words() {
        let v = parse("root { a b }").unwrap();
        assert_eq!(v.get("root").unwrap().get_str("a"), Some("b"));
    }

    #[test]
    fn rejects_malformed_input_without_panicking() {
        for bad in ["\"a\" {", "\"a\" \"b", "}", "\"a\"", "{ }", "/ x", "\"a\" { \"b\" }", "\"a\" \"b\\"] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        let deep = "\"a\" {".repeat(200);
        assert!(parse(&deep).is_err());
    }

    #[test]
    fn empty_document_is_empty_object() {
        assert_eq!(parse("  \n").unwrap(), Vdf::Obj(vec![]));
    }
}

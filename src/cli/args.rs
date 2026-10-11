//! 手写 arg parser（零 dep）：'subcommand [positional] --key value --key=value --flag'.

pub struct Parsed {
    pub positional: Vec<String>,
    pub opts: Vec<(String, String)>,
    pub flags: Vec<String>,
}

impl Parsed {
    pub fn opt(self: &Parsed, name: &str) -> Option<String> {
        for e in &self.opts {
            if e.0 == name {
                return Some(e.1.clone());
            }
        }
        return None;
    }

    pub fn opt_or(self: &Parsed, name: &str, dflt: &str) -> String {
        match self.opt(name) {
            Some(v) => { return v; }
            None => { return dflt.to_string(); }
        }
    }

    pub fn has_flag(self: &Parsed, name: &str) -> bool {
        for f in &self.flags {
            if f == name {
                return true;
            }
        }
        return false;
    }
}

/// '--key value' / '--key=value' -> opts；bare '--key' -> flags；'-x' -> flag；'--' -> rest positional.
pub fn parse(argv: Vec<String>) -> Parsed {
    let mut p = Parsed { positional: Vec::new(), opts: Vec::new(), flags: Vec::new() };
    let mut i: usize = 0;
    while i < argv.len() {
        let a: &str = &argv[i];
        if a == "--" {
            let mut j: usize = i + 1;
            while j < argv.len() {
                p.positional.push(argv[j].to_string());
                j += 1;
            }
            break;
        }
        if a.starts_with("--") && a.len() > 2 {
            let body: &str = &a[2..];
            match body.find("=") {
                Some(k) => { p.opts.push((body[..k].to_string(), body[k + 1..].to_string())); }
                None => {
                    if i + 1 < argv.len() && value_like(&argv[i + 1]) {
                        p.opts.push((body.to_string(), argv[i + 1].to_string()));
                        i += 1;
                    } else {
                        p.flags.push(body.to_string());
                    }
                }
            }
        } else if a.len() >= 2 && a.starts_with("-") {
            p.flags.push(a[1..].to_string());
        } else {
            p.positional.push(a.to_string());
        }
        i += 1;
    }
    return p;
}

/// 允许 '--weight -2'（负数仍属于该选项），但 '--engine --dry-run' 不吞 flag.
fn value_like(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let b = s.as_bytes();
    if b[0] != 0x2D {
        return true;
    }
    if b.len() > 1 {
        let c = b[1];
        if (c >= 0x30 && c <= 0x39) || c == 0x2E {
            return true;
        }
    }
    return false;
}

#[cfg(test)]
mod tests {
    use crate::cli::args::parse;

    fn words(v: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in v {
            out.push(s.to_string());
        }
        return out;
    }

    #[test]
    fn kv_and_flags() {
        let p = parse(words(&["feeds", "add", "--url", "https://x", "--dry-run"]));
        assert_eq!(p.positional.len(), 2);
        assert_eq!(p.has_flag("dry-run"), true);
        match p.opt("url") { Some(u) => { assert_eq!(u, "https://x"); } None => { assert!(false); } }
    }

    #[test]
    fn equals_form() {
        let p = parse(words(&["feeds", "list", "--config=config/a.toml"]));
        assert_eq!(p.opt_or("config", "x"), "config/a.toml");
    }

    #[test]
    fn negative_number_is_value() {
        let p = parse(words(&["feeds", "add", "--weight", "-2"]));
        assert_eq!(p.opt_or("weight", ""), "-2");
    }

    #[test]
    fn flag_does_not_swallow_next_flag() {
        let p = parse(words(&["feeds", "add", "--engine", "--dry-run"]));
        assert_eq!(p.has_flag("dry-run"), true);
        assert_eq!(p.opt("engine"), None);
    }
}

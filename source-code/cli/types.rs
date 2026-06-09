#[derive(Debug, Default)]
pub struct GlobalFlags {
    pub user_mode: bool,
    pub arch:      Option<String>,
    pub yes:       bool,
}

impl GlobalFlags {
    pub fn parse(args: &mut Vec<String>) -> Self {
        let mut f = GlobalFlags::default();
        args.retain(|a| {
            if a == "--user" || a == "-U" {
                f.user_mode = true; false
            } else if let Some(arch) = a.strip_prefix("--arch=") {
                f.arch = Some(arch.to_string()); false
            } else if a == "-y" || a == "--yes" {
                f.yes = true; false
            } else { true }
        });
        f
    }
}

pub fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

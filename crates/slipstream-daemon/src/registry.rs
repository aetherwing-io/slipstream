use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

/// Top-level parsed registry (from TOML or compiled default).
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryConfig {
    /// Map from handler name → handler config.
    pub handlers: HashMap<String, HandlerEntry>,
}

/// One handler entry from [handlers.NAME] in TOML.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum HandlerEntry {
    Full(FullHandlerConfig),
    Advisory(AdvisoryConfig),
}

/// Full handler: manages the file lifecycle via external tool.
#[derive(Debug, Clone, Deserialize)]
pub struct FullHandlerConfig {
    /// File extensions this handler owns, e.g. ["xlsx", "xls"]
    pub extensions: Vec<String>,
    pub tool_prefix: String,
    /// Template string: `{path}` is replaced at runtime.
    pub session_open: String,
    pub session_save: String,
    pub help_tool: String,
    pub description: String,
    #[serde(default)]
    pub examples: Vec<String>,
}

/// Advisory handler: guidance only, file is loaded as text.
#[derive(Debug, Clone, Deserialize)]
pub struct AdvisoryConfig {
    pub extensions: Vec<String>,
    #[serde(default)]
    pub advisory: bool,
    pub description: String,
    pub guidance: String,
}

pub struct FormatRegistry {
    /// Extension string (lowercase) → handler name.
    ext_map: HashMap<String, String>,
    /// Handler name → entry.
    handlers: HashMap<String, HandlerEntry>,
}

impl FormatRegistry {
    /// Build from a RegistryConfig.
    pub fn from_config(config: RegistryConfig) -> Self {
        let mut ext_map = HashMap::new();
        let mut handlers = HashMap::new();
        for (name, entry) in config.handlers {
            let extensions = match &entry {
                HandlerEntry::Full(cfg) => cfg.extensions.clone(),
                HandlerEntry::Advisory(cfg) => cfg.extensions.clone(),
            };
            for ext in &extensions {
                // Store original for exact filename lookup (Makefile, Dockerfile)
                ext_map.insert(ext.clone(), name.clone());
                // Also store lowercased for case-insensitive extension lookup
                ext_map.insert(ext.to_lowercase(), name.clone());
            }
            handlers.insert(name, entry);
        }
        Self { ext_map, handlers }
    }

    /// Return compiled-in default (FCP formats + advisory).
    pub fn default_registry() -> Self {
        let mut handlers = HashMap::new();

        handlers.insert("sheets".to_string(), HandlerEntry::Full(FullHandlerConfig {
            extensions: vec!["xlsx".into(), "xls".into()],
            tool_prefix: "sheets".into(),
            session_open: "sheets_session(\"open {path}\")".into(),
            session_save: "sheets_session(\"save\")".into(),
            help_tool: "sheets_help".into(),
            description: "Excel/spreadsheet file managed by the FCP Sheets server".into(),
            examples: vec![
                "sheets_session(\"open report.xlsx\")".into(),
                "sheets(\"set A1 100\")".into(),
            ],
        }));

        handlers.insert("drawio".to_string(), HandlerEntry::Full(FullHandlerConfig {
            extensions: vec!["drawio".into(), "dio".into()],
            tool_prefix: "drawio".into(),
            session_open: "drawio_session(\"open {path}\")".into(),
            session_save: "drawio_session(\"save\")".into(),
            help_tool: "drawio_help".into(),
            description: "draw.io diagram file managed by the FCP draw.io server".into(),
            examples: vec![
                "drawio_session(\"open arch.drawio\")".into(),
                "drawio(\"add svc AuthService\")".into(),
            ],
        }));

        handlers.insert("midi".to_string(), HandlerEntry::Full(FullHandlerConfig {
            extensions: vec!["mid".into(), "midi".into()],
            tool_prefix: "midi".into(),
            session_open: "midi_session(\"open {path}\")".into(),
            session_save: "midi_session(\"save\")".into(),
            help_tool: "midi_help".into(),
            description: "MIDI file managed by the FCP MIDI server".into(),
            examples: vec![
                "midi_session(\"open song.mid\")".into(),
                "midi(\"note Piano C4 at:1.1 dur:quarter\")".into(),
            ],
        }));

        handlers.insert("terraform".to_string(), HandlerEntry::Full(FullHandlerConfig {
            extensions: vec!["tf".into(), "tfvars".into()],
            tool_prefix: "terraform".into(),
            session_open: "terraform_session(\"open {path}\")".into(),
            session_save: "terraform_session(\"save\")".into(),
            help_tool: "terraform_help".into(),
            description: "Terraform file managed by the FCP Terraform server".into(),
            examples: vec![
                "terraform_session(\"open main.tf\")".into(),
                "terraform(\"add resource aws_instance web\")".into(),
            ],
        }));

        handlers.insert("make".to_string(), HandlerEntry::Advisory(AdvisoryConfig {
            extensions: vec!["Makefile".into(), "makefile".into(), "GNUmakefile".into()],
            advisory: true,
            description: "Makefile — loaded as text but with guidance".into(),
            guidance: "This is a Makefile. Edit it as text using file.str_replace. Run 'make' via Bash after saving. Be careful with tab indentation (Makefiles require real tab characters, not spaces).".into(),
        }));

        handlers.insert("docker".to_string(), HandlerEntry::Advisory(AdvisoryConfig {
            extensions: vec!["Dockerfile".into()],
            advisory: true,
            description: "Dockerfile — loaded as text but with guidance".into(),
            guidance: "This is a Dockerfile. Edit it as text using file.str_replace. Build with 'docker build' via Bash after saving.".into(),
        }));

        Self::from_config(RegistryConfig { handlers })
    }

    /// Load from a TOML file, falling back to default on error.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|s| toml::from_str::<RegistryConfig>(&s).map_err(|e| e.to_string()))
        {
            Ok(config) => Self::from_config(config),
            Err(e) => {
                tracing::warn!("failed to load format registry from {:?}: {}", path, e);
                Self::default_registry()
            }
        }
    }

    /// Look up extension (no leading dot, lowercased) → Option<&HandlerEntry>.
    pub fn lookup_ext(&self, ext: &str) -> Option<&HandlerEntry> {
        let lower = ext.to_lowercase();
        self.ext_map.get(&lower).and_then(|name| self.handlers.get(name))
    }

    /// Look up by filename (handles Makefile, Dockerfile with no extension).
    pub fn lookup_filename(&self, filename: &str) -> Option<&HandlerEntry> {
        // First try exact filename match (handles Makefile, Dockerfile)
        if let Some(entry) = self.ext_map.get(filename).and_then(|n| self.handlers.get(n)) {
            return Some(entry);
        }
        // Fall back to extension
        if let Some(ext) = Path::new(filename).extension() {
            self.lookup_ext(&ext.to_string_lossy())
        } else {
            None
        }
    }
}

impl FullHandlerConfig {
    /// Replaces `{path}` in `self.session_open` with path.display().to_string()
    pub fn interpolate_path(&self, path: &Path) -> String {
        self.session_open.replace("{path}", &path.display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_default_registry_lookup_sheets() {
        let reg = FormatRegistry::default_registry();
        let entry = reg.lookup_ext("xlsx").expect("xlsx should resolve");
        match entry {
            HandlerEntry::Full(cfg) => {
                assert_eq!(cfg.tool_prefix, "sheets");
                assert!(cfg.session_open.contains("{path}"));
                assert!(!cfg.examples.is_empty());
            }
            _ => panic!("expected Full handler for xlsx"),
        }
    }

    #[test]
    fn test_default_registry_lookup_unknown() {
        let reg = FormatRegistry::default_registry();
        assert!(reg.lookup_ext("py").is_none());
        assert!(reg.lookup_ext("rs").is_none());
        assert!(reg.lookup_ext("txt").is_none());
    }

    #[test]
    fn test_default_registry_advisory_makefile() {
        let reg = FormatRegistry::default_registry();
        let entry = reg.lookup_filename("Makefile").expect("Makefile should resolve");
        match entry {
            HandlerEntry::Advisory(cfg) => {
                assert!(cfg.advisory);
                assert!(cfg.guidance.to_lowercase().contains("make"));
            }
            _ => panic!("expected Advisory handler for Makefile"),
        }
    }

    #[test]
    fn test_default_registry_all_fcp_formats() {
        let reg = FormatRegistry::default_registry();
        for ext in &["drawio", "dio", "mid", "midi", "tf", "tfvars", "xls"] {
            assert!(
                matches!(reg.lookup_ext(ext), Some(HandlerEntry::Full(_))),
                "expected Full handler for {ext}"
            );
        }
    }

    #[test]
    fn test_toml_parse_custom_handler() {
        let toml_str = r#"
[handlers.myformat]
extensions = ["myf"]
tool_prefix = "myformat"
session_open = 'myformat_session("open {path}")'
session_save = 'myformat_session("save")'
help_tool = "myformat_help"
description = "My custom format"
examples = []
"#;
        let config: RegistryConfig = toml::from_str(toml_str).unwrap();
        assert!(config.handlers.contains_key("myformat"));
        match &config.handlers["myformat"] {
            HandlerEntry::Full(cfg) => {
                assert!(cfg.extensions.contains(&"myf".to_string()));
            }
            _ => panic!("expected Full handler for myformat"),
        }
    }

    #[test]
    fn test_load_falls_back_to_default_on_missing_file() {
        let reg = FormatRegistry::load(Path::new("/nonexistent/path/formats.toml"));
        assert!(reg.lookup_ext("xlsx").is_some());
    }

    #[test]
    fn test_interpolate_path() {
        let reg = FormatRegistry::default_registry();
        let entry = reg.lookup_ext("xlsx").expect("xlsx should resolve");
        match entry {
            HandlerEntry::Full(cfg) => {
                let result = cfg.interpolate_path(Path::new("/home/user/report.xlsx"));
                assert!(result.contains("/home/user/report.xlsx"));
                assert!(!result.contains("{path}"));
            }
            _ => panic!("expected Full handler"),
        }
    }

    #[test]
    fn test_extension_case_insensitive() {
        let reg = FormatRegistry::default_registry();
        let upper = reg.lookup_ext("XLSX");
        let lower = reg.lookup_ext("xlsx");
        assert!(upper.is_some());
        assert!(lower.is_some());
        // Both should resolve to sheets
        match (upper.unwrap(), lower.unwrap()) {
            (HandlerEntry::Full(a), HandlerEntry::Full(b)) => {
                assert_eq!(a.tool_prefix, b.tool_prefix);
            }
            _ => panic!("expected Full handlers"),
        }
    }
}

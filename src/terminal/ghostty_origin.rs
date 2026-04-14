use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginTerminal {
    pub id: String,
    pub source: OriginSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginSource {
    CliArg,
    SavedFile,
    ProjectDirectory,
    FrontWindow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalInfo {
    id: String,
    working_directory: String,
}

pub fn resolve_origin_terminal(
    project_dir: &Path,
    explicit: Option<&str>,
) -> Result<Option<OriginTerminal>> {
    let terminals = list_terminals()?;

    if let Some(id) = explicit
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .filter(|id| terminal_exists(&terminals, id))
    {
        return Ok(Some(OriginTerminal {
            id: id.to_string(),
            source: OriginSource::CliArg,
        }));
    }

    if let Some(id) =
        read_origin_terminal_id(project_dir).filter(|id| terminal_exists(&terminals, id))
    {
        return Ok(Some(OriginTerminal {
            id,
            source: OriginSource::SavedFile,
        }));
    }

    if let Some(id) = select_project_terminal(project_dir, &terminals) {
        return Ok(Some(OriginTerminal {
            id,
            source: OriginSource::ProjectDirectory,
        }));
    }

    let front = focused_terminal_id()?;
    if front.is_empty() {
        return Ok(None);
    }

    Ok(Some(OriginTerminal {
        id: front,
        source: OriginSource::FrontWindow,
    }))
}

pub fn persist_origin_terminal(project_dir: &Path, terminal_id: &str) -> Result<()> {
    let orca_dir = project_dir.join(".orca");
    std::fs::create_dir_all(&orca_dir)?;
    std::fs::write(orca_dir.join("origin_terminal_id"), terminal_id)?;
    Ok(())
}

pub fn read_origin_terminal_id(project_dir: &Path) -> Option<String> {
    std::fs::read_to_string(project_dir.join(".orca/origin_terminal_id"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn focused_terminal_id() -> Result<String> {
    run_osascript(
        r#"tell application "Ghostty" to get id of focused terminal of selected tab of front window"#,
    )
}

fn list_terminals() -> Result<Vec<TerminalInfo>> {
    let output = run_osascript(
        r#"tell application "Ghostty"
set rows to {}
repeat with t in terminals
    set termId to id of t
    set termWd to working directory of t
    if termWd is missing value then set termWd to ""
    set end of rows to (termId & tab & termWd)
end repeat
set AppleScript's text item delimiters to linefeed
set joined to rows as text
set AppleScript's text item delimiters to ""
return joined
end tell"#,
    )?;

    Ok(output
        .lines()
        .filter_map(|line| {
            let (id, working_directory) = line.split_once('\t')?;
            let id = id.trim();
            if id.is_empty() {
                return None;
            }
            Some(TerminalInfo {
                id: id.to_string(),
                working_directory: working_directory.trim().to_string(),
            })
        })
        .collect())
}

fn terminal_exists(terminals: &[TerminalInfo], id: &str) -> bool {
    terminals.iter().any(|terminal| terminal.id == id)
}

fn select_project_terminal(project_dir: &Path, terminals: &[TerminalInfo]) -> Option<String> {
    let roots = candidate_project_roots(project_dir);
    let exact = collect_matching_ids(terminals, &roots, |wd, root| wd == root);
    if exact.len() == 1 {
        return exact.into_iter().next();
    }

    let nested = collect_matching_ids(terminals, &roots, is_same_project_tree);
    if nested.len() == 1 {
        return nested.into_iter().next();
    }

    None
}

fn collect_matching_ids<F>(
    terminals: &[TerminalInfo],
    roots: &[String],
    matcher: F,
) -> BTreeSet<String>
where
    F: Fn(&str, &str) -> bool,
{
    terminals
        .iter()
        .filter(|terminal| {
            roots
                .iter()
                .any(|root| matcher(terminal.working_directory.as_str(), root))
        })
        .map(|terminal| terminal.id.clone())
        .collect()
}

fn candidate_project_roots(project_dir: &Path) -> Vec<String> {
    let mut roots = BTreeSet::new();
    let raw = project_dir
        .to_string_lossy()
        .trim_end_matches('/')
        .to_string();
    if !raw.is_empty() {
        roots.insert(raw.clone());
        roots.insert(toggle_private_prefix(&raw));
    }

    if let Ok(canonical) = std::fs::canonicalize(project_dir) {
        let canonical = canonical
            .to_string_lossy()
            .trim_end_matches('/')
            .to_string();
        if !canonical.is_empty() {
            roots.insert(canonical.clone());
            roots.insert(toggle_private_prefix(&canonical));
        }
    }

    roots.into_iter().collect()
}

fn toggle_private_prefix(path: &str) -> String {
    if let Some(stripped) = path.strip_prefix("/private") {
        stripped.to_string()
    } else {
        format!("/private{path}")
    }
}

fn is_same_project_tree(working_directory: &str, project_root: &str) -> bool {
    let wd = working_directory.trim_end_matches('/');
    let root = project_root.trim_end_matches('/');
    wd == root || wd.starts_with(&format!("{root}/"))
}

fn run_osascript(script: &str) -> Result<String> {
    let output = std::process::Command::new("osascript")
        .args(["-e", script])
        .output()
        .context("failed to run osascript")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Ghostty AppleScript failed (requires Ghostty with AppleScript enabled). Error: {}",
            stderr.trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        candidate_project_roots, is_same_project_tree, select_project_terminal, TerminalInfo,
    };

    #[test]
    fn selects_unique_exact_project_match() {
        let project_dir = std::path::Path::new("/repo/orca");
        let terminals = vec![
            TerminalInfo {
                id: "cc".into(),
                working_directory: "/repo/orca".into(),
            },
            TerminalInfo {
                id: "other".into(),
                working_directory: "/repo/other".into(),
            },
        ];

        assert_eq!(
            select_project_terminal(project_dir, &terminals),
            Some("cc".into())
        );
    }

    #[test]
    fn selects_unique_nested_match_when_exact_is_missing() {
        let project_dir = std::path::Path::new("/repo/orca");
        let terminals = vec![
            TerminalInfo {
                id: "cc".into(),
                working_directory: "/repo/orca/src".into(),
            },
            TerminalInfo {
                id: "other".into(),
                working_directory: "/repo/other".into(),
            },
        ];

        assert_eq!(
            select_project_terminal(project_dir, &terminals),
            Some("cc".into())
        );
    }

    #[test]
    fn keeps_ambiguous_project_matches_unresolved() {
        let project_dir = std::path::Path::new("/repo/orca");
        let terminals = vec![
            TerminalInfo {
                id: "cc".into(),
                working_directory: "/repo/orca".into(),
            },
            TerminalInfo {
                id: "dup".into(),
                working_directory: "/repo/orca".into(),
            },
        ];

        assert_eq!(select_project_terminal(project_dir, &terminals), None);
    }

    #[test]
    fn accepts_private_tmp_aliases() {
        let roots = candidate_project_roots(std::path::Path::new("/tmp/orca"));
        assert!(roots.contains(&"/tmp/orca".to_string()));
        assert!(roots.contains(&"/private/tmp/orca".to_string()));
    }

    #[test]
    fn project_tree_match_requires_path_boundary() {
        assert!(is_same_project_tree("/repo/orca/src", "/repo/orca"));
        assert!(!is_same_project_tree("/repo/orca-2", "/repo/orca"));
    }
}

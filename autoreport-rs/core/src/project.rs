//! Project-scoped report language configuration.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReportLanguage {
    Latex,
    Typst,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectConfig {
    pub report_language: ReportLanguage,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportLanguageInference {
    Latex,
    Typst,
    Empty,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializePolicy {
    CreateMissingOnly,
}
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MaterializeReport {
    pub created: Vec<PathBuf>,
    pub preserved: Vec<PathBuf>,
    pub failed: Vec<String>,
}

pub fn project_config_path(home: &Path, workspace: &Path) -> PathBuf {
    crate::config::workspace_state_dir(home, workspace).join("project.toml")
}
pub fn load_project_config(home: &Path, workspace: &Path) -> Result<Option<ProjectConfig>> {
    let path = project_config_path(home, workspace);
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(Some(
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?,
    ))
}
pub fn save_project_config(home: &Path, workspace: &Path, config: &ProjectConfig) -> Result<()> {
    let path = project_config_path(home, workspace);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_file_name(format!(".project.toml.{}.tmp", uuid::Uuid::new_v4()));
    let result: Result<()> = (|| {
        let contents =
            toml::to_string_pretty(config).context("serializing project configuration")?;
        std::fs::write(&tmp, contents)?;
        #[cfg(windows)]
        {
            let backup = path.with_file_name(format!(".project.toml.{}.bak", uuid::Uuid::new_v4()));
            if path.exists() {
                std::fs::rename(&path, &backup)?;
                if let Err(error) = std::fs::rename(&tmp, &path) {
                    let _ = std::fs::rename(&backup, &path);
                    return Err(error.into());
                }
                let _ = std::fs::remove_file(&backup);
            } else {
                std::fs::rename(&tmp, &path)?;
            }
        }
        #[cfg(not(windows))]
        std::fs::rename(&tmp, &path)?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("publishing {}", path.display()));
    }
    Ok(())
}
pub fn infer_report_language(workspace: &Path) -> ReportLanguageInference {
    let tex = has_extension(&workspace.join("Report"), "tex");
    let typ = has_extension(&workspace.join("Report"), "typ");
    match (tex, typ) {
        (true, false) => ReportLanguageInference::Latex,
        (false, true) => ReportLanguageInference::Typst,
        (false, false) => ReportLanguageInference::Empty,
        (true, true) => ReportLanguageInference::Ambiguous,
    }
}
fn has_extension(dir: &Path, ext: &str) -> bool {
    std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some(ext))
}
pub fn selected_report_language(home: &Path, workspace: &Path) -> Option<ReportLanguage> {
    load_project_config(home, workspace)
        .ok()
        .flatten()
        .map(|c| c.report_language)
}

/// Copy the active language's program-managed defaults into a project without
/// ever replacing user data. The resource tree is intentionally explicit so
/// switching languages cannot accidentally copy files from the other mode.
pub fn prepare_report_resources(
    workspace: &Path,
    resources_home: &Path,
    language: ReportLanguage,
    _policy: MaterializePolicy,
) -> Result<MaterializeReport> {
    let mut report = plan_report_resources(workspace, resources_home, language)?;
    let planned = std::mem::take(&mut report.created);
    for target in planned {
        let rel = target.strip_prefix(workspace).unwrap_or(&target);
        let source_rel = match rel.file_name().and_then(|n| n.to_str()) {
            Some("main.tex") => PathBuf::from("templates/main.tex"),
            Some("main.typ") => PathBuf::from("templates/main.typ"),
            Some("bibli.bib") => PathBuf::from("templates/bibli.bib"),
            Some("american-physics-society.csl") => {
                PathBuf::from("templates/american-physics-society.csl")
            }
            Some("mpltx.cls") => PathBuf::from("themes/mpltx.cls"),
            Some("mplts.typ") => PathBuf::from("themes/mplts.typ"),
            _ => continue,
        };
        let root = match language {
            ReportLanguage::Latex => "latex",
            ReportLanguage::Typst => "typst",
        };
        let source = resources_home.join("resources").join(root).join(source_rel);
        if let Err(e) = std::fs::create_dir_all(target.parent().unwrap()) {
            report.failed.push(format!("{}: {e}", target.display()));
            continue;
        }
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
        {
            Ok(mut destination) => {
                match std::fs::File::open(&source)
                    .and_then(|mut source| std::io::copy(&mut source, &mut destination).map(|_| ()))
                {
                    Ok(()) => report.created.push(target),
                    Err(e) => {
                        let _ = std::fs::remove_file(&target);
                        report.failed.push(format!("{}: {e}", target.display()));
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => report.preserved.push(target),
            Err(e) => report.failed.push(format!("{}: {e}", target.display())),
        }
    }
    Ok(report)
}

/// Read-only resource plan used by UI review pages. This function never
/// creates directories or files.
pub fn plan_report_resources(
    workspace: &Path,
    resources_home: &Path,
    language: ReportLanguage,
) -> Result<MaterializeReport> {
    let (root, files): (&str, &[&str]) = match language {
        ReportLanguage::Latex => ("latex", &["templates/main.tex", "themes/mpltx.cls"]),
        ReportLanguage::Typst => (
            "typst",
            &[
                "templates/main.typ",
                "templates/bibli.bib",
                "templates/american-physics-society.csl",
                "themes/mplts.typ",
            ],
        ),
    };
    let mut report = MaterializeReport::default();
    for rel in files {
        let source = resources_home.join("resources").join(root).join(rel);
        let target_name = match *rel {
            "templates/main.tex" => "main.tex",
            "templates/main.typ" => "main.typ",
            x if x.starts_with("templates/") => x.trim_start_matches("templates/"),
            x => x.trim_start_matches("themes/"),
        };
        let target = workspace.join("Report").join(target_name);
        if target.exists() {
            report.preserved.push(target);
            continue;
        }
        if !source.is_file() {
            report
                .failed
                .push(format!("missing bundled resource {}", source.display()));
            continue;
        }
        report.created.push(target);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    #[test]
    fn infers_only_report_entries() {
        let d = tempdir().unwrap();
        let tex = d.path().join("Report");
        std::fs::create_dir(&tex).unwrap();
        std::fs::write(tex.join("main.tex"), "").unwrap();
        assert_eq!(
            infer_report_language(d.path()),
            ReportLanguageInference::Latex
        );
        std::fs::write(tex.join("main.typ"), "").unwrap();
        assert_eq!(
            infer_report_language(d.path()),
            ReportLanguageInference::Ambiguous
        );
    }
    #[test]
    fn project_config_is_atomic_and_roundtrips() {
        let h = tempdir().unwrap();
        let w = tempdir().unwrap();
        let c = ProjectConfig {
            report_language: ReportLanguage::Typst,
        };
        save_project_config(h.path(), w.path(), &c).unwrap();
        assert_eq!(load_project_config(h.path(), w.path()).unwrap(), Some(c));
    }
    #[test]
    fn malformed_project_config_is_reported() {
        let h = tempdir().unwrap();
        let w = tempdir().unwrap();
        let path = project_config_path(h.path(), w.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "report_language = 'invalid'").unwrap();
        assert!(load_project_config(h.path(), w.path()).is_err());
    }
    #[test]
    fn resource_preparation_preserves_existing_files() {
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        crate::bundled::materialize(home.path());
        let tex = workspace.path().join("Report");
        std::fs::create_dir_all(&tex).unwrap();
        std::fs::write(tex.join("main.typ"), "user edit").unwrap();
        let first = prepare_report_resources(
            workspace.path(),
            home.path(),
            ReportLanguage::Typst,
            MaterializePolicy::CreateMissingOnly,
        )
        .unwrap();
        assert!(first.preserved.iter().any(|p| p.ends_with("main.typ")));
        assert_eq!(
            std::fs::read_to_string(tex.join("main.typ")).unwrap(),
            "user edit"
        );
        assert!(tex.join("mplts.typ").exists());
    }

    #[test]
    fn latex_resource_preparation_uses_bundled_cjk_theme() {
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        crate::bundled::materialize(home.path());

        let report = prepare_report_resources(
            workspace.path(),
            home.path(),
            ReportLanguage::Latex,
            MaterializePolicy::CreateMissingOnly,
        )
        .unwrap();

        assert!(report.failed.is_empty());
        let tex = workspace.path().join("Report");
        let main = std::fs::read_to_string(tex.join("main.tex")).unwrap();
        assert!(main.contains(r"\documentclass[font=macos]{mpltx}"));
        assert!(tex.join("mpltx.cls").is_file());
    }

    #[test]
    fn resource_preparation_does_not_overwrite_after_planning() {
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        crate::bundled::materialize(home.path());
        let plan =
            plan_report_resources(workspace.path(), home.path(), ReportLanguage::Typst).unwrap();
        assert!(plan.created.iter().any(|p| p.ends_with("main.typ")));
        let tex = workspace.path().join("Report");
        std::fs::create_dir_all(&tex).unwrap();
        std::fs::write(tex.join("main.typ"), "created by another writer").unwrap();
        let result = prepare_report_resources(
            workspace.path(),
            home.path(),
            ReportLanguage::Typst,
            MaterializePolicy::CreateMissingOnly,
        )
        .unwrap();
        assert!(result.preserved.iter().any(|p| p.ends_with("main.typ")));
        assert_eq!(
            std::fs::read_to_string(tex.join("main.typ")).unwrap(),
            "created by another writer"
        );
    }
}

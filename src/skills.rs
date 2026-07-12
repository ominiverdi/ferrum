use crate::terminal_text;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const MAX_SKILLS: usize = 256;
const MAX_SKILL_DIRECTORIES: usize = 1_024;
const MAX_SKILL_DIRECTORY_ENTRIES: usize = 4_096;
const MAX_SKILL_DEPTH: usize = 16;
const MAX_SKILL_FRONTMATTER_BYTES: usize = 16 * 1024;
const MAX_SKILL_FILE_BYTES: usize = 256 * 1024;
const MAX_SKILL_DESCRIPTION_BYTES: usize = 1_024;
const MAX_GITDIR_FILE_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub dir: PathBuf,
    pub(crate) approved_root: PathBuf,
    pub(crate) external_allowed: bool,
}

pub fn discover(
    config_dir: &Path,
    cwd: &Path,
    allow_external_global_symlinks: bool,
    inherit_global: bool,
    allow: Option<&[String]>,
    deny: &[String],
) -> Result<Vec<Skill>> {
    discover_with_home_policy(
        config_dir,
        &home_dir()?,
        cwd,
        allow_external_global_symlinks,
        inherit_global,
        allow,
        deny,
    )
}

#[cfg(test)]
fn discover_with_home(
    config_dir: &Path,
    home: &Path,
    cwd: &Path,
    allow_external_global_symlinks: bool,
) -> Result<Vec<Skill>> {
    discover_with_home_policy(
        config_dir,
        home,
        cwd,
        allow_external_global_symlinks,
        true,
        None,
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
fn discover_with_home_policy(
    config_dir: &Path,
    home: &Path,
    cwd: &Path,
    allow_external_global_symlinks: bool,
    inherit_global: bool,
    allow: Option<&[String]>,
    deny: &[String],
) -> Result<Vec<Skill>> {
    let mut discovery = Discovery::default();

    if inherit_global {
        discovery.add_root(
            &config_dir.join("skills"),
            true,
            allow_external_global_symlinks,
        )?;
        discovery.add_root(
            &home.join(".agents/skills"),
            false,
            allow_external_global_symlinks,
        )?;
    }

    let mut ancestors = project_ancestors(cwd);
    ancestors.reverse();
    for dir in ancestors {
        // Repository-controlled skill links never escape their declared project root.
        discovery.add_root(&dir.join(".ferrum/skills"), true, false)?;
        discovery.add_root(&dir.join(".agents/skills"), false, false)?;
    }

    let allow = allow.map(|names| names.iter().map(String::as_str).collect::<HashSet<_>>());
    let deny = deny.iter().map(String::as_str).collect::<HashSet<_>>();
    Ok(dedup_project_overrides(discovery.skills)
        .into_iter()
        .filter(|skill| {
            allow
                .as_ref()
                .is_none_or(|allow| allow.contains(skill.name.as_str()))
                && !deny.contains(skill.name.as_str())
        })
        .collect())
}

#[derive(Default)]
struct Discovery {
    skills: Vec<Skill>,
    visited_dirs: HashSet<PathBuf>,
    directory_count: usize,
}

impl Discovery {
    fn add_root(&mut self, dir: &Path, direct_md: bool, allow_external: bool) -> Result<()> {
        let Ok(root_metadata) = fs::symlink_metadata(dir) else {
            return Ok(());
        };
        if !root_metadata.file_type().is_dir() && !root_metadata.file_type().is_symlink() {
            return Ok(());
        }
        if root_metadata.file_type().is_symlink() && !allow_external {
            anyhow::bail!(
                "skill root symlink requires skills.allow_external_global_symlinks=true: {}",
                dir.display()
            );
        }
        let approved_root = fs::canonicalize(dir)
            .with_context(|| format!("failed to resolve skill root {}", dir.display()))?;
        if !approved_root.is_dir() {
            return Ok(());
        }

        let start = self.skills.len();
        self.visit_dir(&approved_root, &approved_root, direct_md, allow_external, 0)?;
        reject_duplicate_skills_same_scope(&self.skills[start..], dir)
    }

    fn visit_dir(
        &mut self,
        dir: &Path,
        approved_root: &Path,
        direct_md: bool,
        allow_external: bool,
        depth: usize,
    ) -> Result<()> {
        if depth > MAX_SKILL_DEPTH {
            anyhow::bail!(
                "skill discovery depth exceeds {MAX_SKILL_DEPTH} below {}",
                approved_root.display()
            );
        }
        let canonical = fs::canonicalize(dir)
            .with_context(|| format!("failed to resolve skill directory {}", dir.display()))?;
        ensure_skill_containment(&canonical, approved_root, allow_external)?;
        if !self.visited_dirs.insert(canonical.clone()) {
            anyhow::bail!(
                "skill directory cycle or repeated canonical directory: {}",
                canonical.display()
            );
        }
        self.directory_count += 1;
        if self.directory_count > MAX_SKILL_DIRECTORIES {
            anyhow::bail!("skill discovery exceeds {MAX_SKILL_DIRECTORIES} directories");
        }

        let entries = sorted_dir_entries(&canonical)?;
        if direct_md {
            for path in &entries {
                if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                    continue;
                }
                let Some(canonical_file) = canonical_regular_file(path)? else {
                    continue;
                };
                ensure_skill_containment(&canonical_file, approved_root, allow_external)?;
                self.add_skill_file(&canonical_file, approved_root, allow_external)?;
            }
        }

        for path in entries {
            let Some(canonical_dir) = canonical_directory(&path)? else {
                continue;
            };
            ensure_skill_containment(&canonical_dir, approved_root, allow_external)?;
            let skill_path = canonical_dir.join("SKILL.md");
            if let Some(canonical_skill) = canonical_regular_file(&skill_path)? {
                ensure_skill_containment(&canonical_skill, approved_root, allow_external)?;
                self.add_skill_file(&canonical_skill, approved_root, allow_external)?;
            } else {
                self.visit_dir(
                    &canonical_dir,
                    approved_root,
                    false,
                    allow_external,
                    depth + 1,
                )?;
            }
        }
        Ok(())
    }

    fn add_skill_file(
        &mut self,
        path: &Path,
        approved_root: &Path,
        allow_external: bool,
    ) -> Result<()> {
        if self.skills.len() >= MAX_SKILLS {
            anyhow::bail!("skill discovery exceeds {MAX_SKILLS} skills");
        }
        if let Some(skill) = parse_skill_file(path, approved_root, allow_external)? {
            self.skills.push(skill);
        }
        Ok(())
    }
}

pub fn render_available_skills(skills: &[Skill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut output = String::from(
        "Available skills are listed below. Skills are specialized instruction packages. If a task matches a skill, load the full skill file with the read tool before using it. Skill-relative files should be resolved relative to the skill directory.\n\n<available_skills>\n",
    );
    for skill in skills {
        output.push_str("  <skill>\n");
        output.push_str(&format!("    <name>{}</name>\n", escape_xml(&skill.name)));
        output.push_str(&format!(
            "    <description>{}</description>\n",
            escape_xml(&skill.description)
        ));
        output.push_str(&format!(
            "    <path>{}</path>\n",
            escape_xml(&skill.path.display().to_string())
        ));
        output.push_str(&format!(
            "    <dir>{}</dir>\n",
            escape_xml(&skill.dir.display().to_string())
        ));
        output.push_str("  </skill>\n");
    }
    output.push_str("</available_skills>");
    Some(output)
}

pub fn expand_skill_prompt(skill: &Skill, args: Option<&str>) -> Result<String> {
    let mut file = open_verified_skill(skill)?;
    let content = read_utf8_bounded(
        &mut file,
        MAX_SKILL_FILE_BYTES,
        &format!("skill {}", skill.path.display()),
    )?;
    let body = strip_frontmatter(&content).trim();
    let skill_block = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        escape_xml(&skill.name),
        escape_xml(&skill.path.display().to_string()),
        escape_xml(&skill.dir.display().to_string()),
        body
    );
    if let Some(args) = args.filter(|args| !args.trim().is_empty()) {
        Ok(format!("{skill_block}\n\n{}", args.trim()))
    } else {
        Ok(skill_block)
    }
}

fn open_verified_skill(skill: &Skill) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&skill.path)
        .with_context(|| format!("failed to open skill {}", skill.path.display()))?;
    let opened = file
        .metadata()
        .with_context(|| format!("failed to inspect skill {}", skill.path.display()))?;
    if !opened.is_file() {
        anyhow::bail!("skill is not a regular file: {}", skill.path.display());
    }
    let canonical = fs::canonicalize(&skill.path)
        .with_context(|| format!("failed to resolve skill {}", skill.path.display()))?;
    ensure_skill_containment(&canonical, &skill.approved_root, skill.external_allowed)?;
    let current = fs::metadata(&canonical)
        .with_context(|| format!("failed to inspect skill {}", canonical.display()))?;
    if opened.dev() != current.dev() || opened.ino() != current.ino() {
        anyhow::bail!("skill changed while opening: {}", skill.path.display());
    }
    Ok(file)
}

fn canonical_regular_file(path: &Path) -> Result<Option<PathBuf>> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(None);
    };
    if !metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
        return Ok(None);
    }
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve skill candidate {}", path.display()))?;
    Ok(canonical.is_file().then_some(canonical))
}

fn canonical_directory(path: &Path) -> Result<Option<PathBuf>> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(None);
    };
    if !metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        return Ok(None);
    }
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve skill directory {}", path.display()))?;
    Ok(canonical.is_dir().then_some(canonical))
}

fn ensure_skill_containment(path: &Path, approved_root: &Path, allow_external: bool) -> Result<()> {
    if allow_external || path.starts_with(approved_root) {
        return Ok(());
    }
    anyhow::bail!(
        "skill path escapes approved root {}: {}",
        approved_root.display(),
        path.display()
    )
}

fn sorted_dir_entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        entries.push(entry?.path());
        if entries.len() > MAX_SKILL_DIRECTORY_ENTRIES {
            anyhow::bail!(
                "skill directory {} exceeds {MAX_SKILL_DIRECTORY_ENTRIES} entries",
                dir.display()
            );
        }
    }
    entries.sort();
    Ok(entries)
}

fn reject_duplicate_skills_same_scope(skills: &[Skill], scope: &Path) -> Result<()> {
    let mut seen = HashMap::new();
    for skill in skills {
        if let Some(previous) = seen.insert(skill.name.clone(), skill.path.clone()) {
            anyhow::bail!(
                "duplicate skill `{}` in {}: {} and {}",
                skill.name,
                scope.display(),
                previous.display(),
                skill.path.display()
            );
        }
    }
    Ok(())
}

fn parse_skill_file(
    path: &Path,
    approved_root: &Path,
    allow_external: bool,
) -> Result<Option<Skill>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let prefix = read_utf8_prefix(
        &mut file,
        MAX_SKILL_FRONTMATTER_BYTES,
        &format!("skill frontmatter in {}", path.display()),
    )?;
    let Some(frontmatter) = extract_frontmatter(&prefix) else {
        if file
            .metadata()
            .is_ok_and(|metadata| metadata.len() > MAX_SKILL_FRONTMATTER_BYTES as u64)
        {
            anyhow::bail!(
                "skill frontmatter in {} exceeds {MAX_SKILL_FRONTMATTER_BYTES} bytes",
                path.display()
            );
        }
        eprintln!(
            "[skills] skipping {}: missing bounded frontmatter",
            terminal_text::sanitize(&path.display().to_string())
        );
        return Ok(None);
    };
    let fields = match parse_frontmatter_fields(&frontmatter) {
        Ok(fields) => fields,
        Err(error) => {
            eprintln!(
                "[skills] skipping {}: invalid frontmatter: {}",
                terminal_text::sanitize(&path.display().to_string()),
                terminal_text::sanitize(&error.to_string())
            );
            return Ok(None);
        }
    };
    let Some(name) = fields.get("name").cloned() else {
        eprintln!(
            "[skills] skipping {}: missing name",
            terminal_text::sanitize(&path.display().to_string())
        );
        return Ok(None);
    };
    let Some(description) = fields.get("description").cloned() else {
        eprintln!(
            "[skills] skipping {}: missing description",
            terminal_text::sanitize(&path.display().to_string())
        );
        return Ok(None);
    };
    if !valid_skill_name(&name) {
        eprintln!(
            "[skills] skipping {}: invalid name `{}`",
            terminal_text::sanitize(&path.display().to_string()),
            terminal_text::sanitize(&name)
        );
        return Ok(None);
    }
    if description.trim().is_empty() || description.len() > MAX_SKILL_DESCRIPTION_BYTES {
        eprintln!(
            "[skills] skipping {}: description must be 1-{MAX_SKILL_DESCRIPTION_BYTES} bytes",
            terminal_text::sanitize(&path.display().to_string())
        );
        return Ok(None);
    }
    let path = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve skill {}", path.display()))?;
    ensure_skill_containment(&path, approved_root, allow_external)?;
    Ok(Some(Skill {
        name,
        description,
        dir: path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
        path,
        approved_root: approved_root.to_path_buf(),
        external_allowed: allow_external,
    }))
}

fn read_utf8_prefix(file: &mut File, limit: usize, label: &str) -> Result<String> {
    let mut bytes = Vec::with_capacity(limit.min(16 * 1024));
    file.take(limit as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {label}"))?;
    String::from_utf8(bytes).with_context(|| format!("{label} is not UTF-8"))
}

fn read_utf8_bounded(file: &mut File, limit: usize, label: &str) -> Result<String> {
    let mut bytes = Vec::with_capacity(limit.min(16 * 1024).saturating_add(1));
    file.take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {label}"))?;
    if bytes.len() > limit {
        anyhow::bail!("{label} exceeds {limit} bytes");
    }
    String::from_utf8(bytes).with_context(|| format!("{label} is not UTF-8"))
}

fn extract_frontmatter(text: &str) -> Option<String> {
    let (start, end, _) = frontmatter_bounds(text)?;
    Some(text[start..end].replace("\r\n", "\n"))
}

fn strip_frontmatter(text: &str) -> &str {
    let Some((_, _, body_start)) = frontmatter_bounds(text) else {
        return text;
    };
    &text[body_start..]
}

fn frontmatter_bounds(text: &str) -> Option<(usize, usize, usize)> {
    let offset = text
        .strip_prefix('\u{feff}')
        .map_or(0, |_| '\u{feff}'.len_utf8());
    let rest = &text[offset..];
    let opening_len = if rest.starts_with("---\r\n") {
        "---\r\n".len()
    } else if rest.starts_with("---\n") {
        "---\n".len()
    } else {
        return None;
    };
    let body_start = offset + opening_len;
    let body = &text[body_start..];
    let candidates = ["\r\n---\r\n", "\n---\n", "\r\n---\n", "\n---\r\n"];
    let (relative_end, delimiter_len) = candidates
        .iter()
        .filter_map(|delimiter| body.find(delimiter).map(|index| (index, delimiter.len())))
        .min_by_key(|(index, _)| *index)?;
    Some((
        body_start,
        body_start + relative_end,
        body_start + relative_end + delimiter_len,
    ))
}

#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
}

fn parse_frontmatter_fields(frontmatter: &str) -> Result<HashMap<String, String>> {
    let parsed: SkillFrontmatter = serde_yaml::from_str(frontmatter)?;
    let mut fields = HashMap::new();
    if let Some(name) = parsed.name {
        fields.insert("name".to_string(), name);
    }
    if let Some(description) = parsed.description {
        fields.insert("description".to_string(), description);
    }
    Ok(fields)
}

fn valid_skill_name(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 64
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
    {
        return false;
    }
    name.chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

fn dedup_project_overrides(skills: Vec<Skill>) -> Vec<Skill> {
    let mut output: Vec<Skill> = Vec::new();
    for skill in skills {
        if let Some(existing) = output.iter().position(|other| other.name == skill.name) {
            output[existing] = skill;
        } else {
            output.push(skill);
        }
    }
    output
}

fn project_ancestors(cwd: &Path) -> Vec<PathBuf> {
    project_ancestors_with_boundary(cwd, None)
}

fn project_ancestors_with_boundary(cwd: &Path, boundary: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut current = Some(cwd);
    while let Some(dir) = current {
        dirs.push(dir.to_path_buf());
        if is_project_marker(dir) {
            return dirs;
        }
        if boundary == Some(dir) {
            break;
        }
        current = dir.parent();
    }
    vec![cwd.to_path_buf()]
}

fn is_project_marker(dir: &Path) -> bool {
    valid_git_marker(dir)
        || dir.join(".ferrum").is_dir()
        || dir.join("AGENTS.md").is_file()
        || dir.join("agents.md").is_file()
}

fn valid_git_marker(dir: &Path) -> bool {
    let marker = dir.join(".git");
    let Ok(metadata) = fs::symlink_metadata(&marker) else {
        return false;
    };
    if metadata.file_type().is_dir() {
        return marker.join("HEAD").is_file();
    }
    if !metadata.file_type().is_file() {
        return false;
    }
    let Ok(mut file) = File::open(&marker) else {
        return false;
    };
    let Ok(text) = read_utf8_bounded(&mut file, MAX_GITDIR_FILE_BYTES, ".git file") else {
        return false;
    };
    let Some(target) = text.trim().strip_prefix("gitdir:") else {
        return false;
    };
    let target = target.trim();
    if target.is_empty() {
        return false;
    }
    let git_dir = if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else {
        dir.join(target)
    };
    git_dir.join("HEAD").is_file()
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, os::unix::fs::symlink};

    fn write_skill(path: &Path, name: &str, description: &str, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = File::create(path).unwrap();
        writeln!(
            file,
            "---\nname: {name}\ndescription: {description}\n---\n{body}"
        )
        .unwrap();
    }

    fn parse_local_skill(path: &Path) -> Skill {
        let root = fs::canonicalize(path.parent().unwrap()).unwrap();
        let path = fs::canonicalize(path).unwrap();
        parse_skill_file(&path, &root, false).unwrap().unwrap()
    }

    #[test]
    fn validates_skill_names() {
        assert!(valid_skill_name("pdf-tools"));
        assert!(!valid_skill_name("PDF"));
        assert!(!valid_skill_name("-pdf"));
        assert!(!valid_skill_name("pdf--tools"));
    }

    #[test]
    fn project_policy_can_exclude_global_skills_and_filter_project_skills() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        fs::create_dir_all(project.join(".ferrum")).unwrap();
        write_skill(
            &config.join("skills/global/SKILL.md"),
            "global",
            "global skill",
            "global",
        );
        write_skill(
            &project.join(".ferrum/skills/allowed/SKILL.md"),
            "allowed",
            "allowed skill",
            "allowed",
        );
        write_skill(
            &project.join(".ferrum/skills/denied/SKILL.md"),
            "denied",
            "denied skill",
            "denied",
        );

        let skills = discover_with_home_policy(
            &config,
            &home,
            &project,
            false,
            false,
            Some(&["allowed".to_string(), "global".to_string()]),
            &["denied".to_string()],
        )
        .unwrap();

        assert_eq!(
            skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            ["allowed"]
        );
    }

    #[test]
    fn parses_frontmatter() {
        let fm = extract_frontmatter("---\nname: test\ndescription: hello\n---\nbody").unwrap();
        let fields = parse_frontmatter_fields(&fm).unwrap();
        assert_eq!(fields.get("name").unwrap(), "test");
        assert_eq!(fields.get("description").unwrap(), "hello");
    }

    #[test]
    fn outside_project_does_not_load_parent_skills() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path();
        let cwd = parent.join("child");
        fs::create_dir_all(&cwd).unwrap();

        let ancestors = project_ancestors_with_boundary(&cwd, Some(parent));

        assert_eq!(ancestors, vec![cwd]);
    }

    #[test]
    fn empty_git_path_is_not_a_project_marker() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path();
        let cwd = parent.join("child");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir(parent.join(".git")).unwrap();

        let ancestors = project_ancestors_with_boundary(&cwd, Some(parent));

        assert_eq!(ancestors, vec![cwd]);
    }

    #[test]
    fn real_git_directory_is_a_project_marker() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let nested = repo.join("a/b");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        assert_eq!(
            project_ancestors(&nested),
            vec![nested, repo.join("a"), repo]
        );
    }

    #[test]
    fn project_ancestors_stop_at_instruction_marker() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let nested = repo.join("a/b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(repo.join("AGENTS.md"), "instructions").unwrap();

        let ancestors = project_ancestors(&nested);

        assert_eq!(ancestors, vec![nested, repo.join("a"), repo]);
    }

    #[test]
    fn duplicate_skill_name_same_dir_errors_deterministically() {
        let temp = tempfile::tempdir().unwrap();
        let skills_dir = temp.path().join("skills");
        write_skill(&skills_dir.join("a/SKILL.md"), "dup", "first", "body");
        write_skill(&skills_dir.join("b/SKILL.md"), "dup", "second", "body");
        let mut discovery = Discovery::default();

        let error = discovery.add_root(&skills_dir, true, false).unwrap_err();

        assert!(error.to_string().contains("duplicate skill `dup`"));
        assert!(error.to_string().contains("a/SKILL.md"));
        assert!(error.to_string().contains("b/SKILL.md"));
    }

    #[test]
    fn skill_with_bom_frontmatter_loads() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("SKILL.md");
        fs::write(
            &path,
            "\u{feff}---\nname: bom-skill\ndescription: loads\n---\nbody\n",
        )
        .unwrap();

        let skill = parse_local_skill(&path);

        assert_eq!(skill.name, "bom-skill");
        assert_eq!(
            strip_frontmatter(&fs::read_to_string(&path).unwrap()).trim(),
            "body"
        );
    }

    #[test]
    fn skill_with_crlf_frontmatter_loads() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("SKILL.md");
        fs::write(
            &path,
            "---\r\nname: crlf-skill\r\ndescription: loads\r\n---\r\nbody\r\n",
        )
        .unwrap();

        let skill = parse_local_skill(&path);

        assert_eq!(skill.name, "crlf-skill");
    }

    #[test]
    fn skill_frontmatter_uses_yaml_colon_strings() {
        let fm = "name: yaml-skill\ndescription: \"does: parse: colons\"\n";
        let fields = parse_frontmatter_fields(fm).unwrap();
        assert_eq!(fields.get("description").unwrap(), "does: parse: colons");
    }

    #[test]
    fn rejects_self_symlink_cycle() {
        let temp = tempfile::tempdir().unwrap();
        let skills = temp.path().join("skills");
        fs::create_dir_all(&skills).unwrap();
        symlink(&skills, skills.join("self")).unwrap();
        let mut discovery = Discovery::default();

        let error = discovery.add_root(&skills, true, false).unwrap_err();

        assert!(error.to_string().contains("cycle or repeated canonical"));
    }

    #[test]
    fn rejects_parent_symlink_cycle() {
        let temp = tempfile::tempdir().unwrap();
        let skills = temp.path().join("skills");
        fs::create_dir_all(skills.join("nested")).unwrap();
        symlink(&skills, skills.join("nested/back")).unwrap();
        let mut discovery = Discovery::default();

        let error = discovery.add_root(&skills, true, false).unwrap_err();

        assert!(error.to_string().contains("cycle or repeated canonical"));
    }

    #[test]
    fn project_skill_symlink_cannot_escape_root() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config");
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        let project_skills = repo.join(".ferrum/skills");
        let external = temp.path().join("external/escaped");
        write_skill(&external.join("SKILL.md"), "escaped", "outside", "body");
        fs::create_dir_all(&project_skills).unwrap();
        symlink(&external, project_skills.join("linked")).unwrap();

        let error = discover_with_home(&config, &home, &repo, true).unwrap_err();

        assert!(error.to_string().contains("escapes approved root"));
    }

    #[test]
    fn global_cross_root_skill_symlink_requires_opt_in() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config");
        let home = temp.path().join("home");
        let cwd = temp.path().join("work");
        let ferrum_global = config.join("skills");
        let agents_global = home.join(".agents/skills/cross-root");
        write_skill(
            &agents_global.join("SKILL.md"),
            "cross-root",
            "other global root",
            "body",
        );
        fs::create_dir_all(&ferrum_global).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        symlink(&agents_global, ferrum_global.join("cross-root")).unwrap();

        let error = discover_with_home(&config, &home, &cwd, false).unwrap_err();

        assert!(error.to_string().contains("escapes approved root"));
    }

    #[test]
    fn global_external_skill_symlink_requires_opt_in() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config");
        let home = temp.path().join("home");
        let cwd = temp.path().join("work");
        let global = config.join("skills");
        let external = temp.path().join("external/linked");
        write_skill(&external.join("SKILL.md"), "linked", "outside", "body");
        fs::create_dir_all(&global).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        symlink(&external, global.join("linked")).unwrap();

        let denied = discover_with_home(&config, &home, &cwd, false).unwrap_err();
        let allowed = discover_with_home(&config, &home, &cwd, true).unwrap();

        assert!(denied.to_string().contains("escapes approved root"));
        assert_eq!(allowed.len(), 1);
        assert_eq!(allowed[0].name, "linked");
    }

    #[test]
    fn frontmatter_read_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("SKILL.md");
        fs::write(
            &path,
            format!(
                "---\nname: huge\ndescription: {}",
                "x".repeat(MAX_SKILL_FRONTMATTER_BYTES)
            ),
        )
        .unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let path = fs::canonicalize(path).unwrap();

        let error = parse_skill_file(&path, &root, false).unwrap_err();

        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn body_is_loaded_only_on_invocation_and_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let skills_dir = temp.path().join("skills");
        write_skill(
            &skills_dir.join("large/SKILL.md"),
            "large",
            "bounded body",
            &"x".repeat(MAX_SKILL_FILE_BYTES),
        );
        let mut discovery = Discovery::default();
        discovery.add_root(&skills_dir, true, false).unwrap();

        let error = expand_skill_prompt(&discovery.skills[0], None).unwrap_err();

        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn discovery_depth_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let skills_dir = temp.path().join("skills");
        let mut deepest = skills_dir.clone();
        for index in 0..=MAX_SKILL_DEPTH {
            deepest = deepest.join(format!("d{index}"));
        }
        fs::create_dir_all(&deepest).unwrap();
        let mut discovery = Discovery::default();

        let error = discovery.add_root(&skills_dir, true, false).unwrap_err();

        assert!(error.to_string().contains("depth exceeds"));
    }

    #[test]
    fn discovery_skill_count_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let skills_dir = temp.path().join("skills");
        for index in 0..=MAX_SKILLS {
            write_skill(
                &skills_dir.join(format!("skill-{index}/SKILL.md")),
                &format!("skill-{index}"),
                "counted",
                "body",
            );
        }
        let mut discovery = Discovery::default();

        let error = discovery.add_root(&skills_dir, true, false).unwrap_err();

        assert!(error.to_string().contains("exceeds 256 skills"));
    }
}

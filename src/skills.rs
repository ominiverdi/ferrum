use anyhow::{Context, Result};
use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub dir: PathBuf,
}

pub fn discover(config_dir: &Path, cwd: &Path) -> Result<Vec<Skill>> {
    let mut skills = Vec::new();

    add_skills_from_dir(&mut skills, &config_dir.join("skills"), true)?;
    add_skills_from_dir(&mut skills, &home_dir()?.join(".agents/skills"), false)?;

    let mut ancestors = project_ancestors(cwd);
    ancestors.reverse();
    for dir in ancestors {
        add_skills_from_dir(&mut skills, &dir.join(".ferrum/skills"), true)?;
        add_skills_from_dir(&mut skills, &dir.join(".agents/skills"), false)?;
    }

    Ok(dedup_project_overrides(skills))
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
    let content = fs::read_to_string(&skill.path)
        .with_context(|| format!("failed to read skill {}", skill.path.display()))?;
    let body = strip_frontmatter(&content).trim();
    let skill_block = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        escape_xml(&skill.name),
        escape_xml(&skill.path.display().to_string()),
        skill.dir.display(),
        body
    );
    if let Some(args) = args.filter(|args| !args.trim().is_empty()) {
        Ok(format!("{skill_block}\n\n{}", args.trim()))
    } else {
        Ok(skill_block)
    }
}

fn add_skills_from_dir(skills: &mut Vec<Skill>, dir: &Path, direct_md: bool) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }

    let start = skills.len();
    if direct_md {
        for path in sorted_dir_entries(dir)? {
            if path.is_file()
                && path.extension().and_then(|ext| ext.to_str()) == Some("md")
                && let Some(skill) = parse_skill_file(&path)?
            {
                skills.push(skill);
            }
        }
    }

    visit_skill_dirs(dir, skills)?;
    reject_duplicate_skills_same_scope(&skills[start..], dir)
}

fn visit_skill_dirs(dir: &Path, skills: &mut Vec<Skill>) -> Result<()> {
    for path in sorted_dir_entries(dir)? {
        if !path.is_dir() {
            continue;
        }
        let skill_path = path.join("SKILL.md");
        if skill_path.is_file() {
            if let Some(skill) = parse_skill_file(&skill_path)? {
                skills.push(skill);
            }
        } else {
            visit_skill_dirs(&path, skills)?;
        }
    }
    Ok(())
}

fn sorted_dir_entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
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

fn parse_skill_file(path: &Path) -> Result<Option<Skill>> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let Some(frontmatter) = extract_frontmatter(&text) else {
        eprintln!("[skills] skipping {}: missing frontmatter", path.display());
        return Ok(None);
    };
    let fields = match parse_frontmatter_fields(&frontmatter) {
        Ok(fields) => fields,
        Err(error) => {
            eprintln!(
                "[skills] skipping {}: invalid frontmatter: {error}",
                path.display()
            );
            return Ok(None);
        }
    };
    let Some(name) = fields.get("name").cloned() else {
        eprintln!("[skills] skipping {}: missing name", path.display());
        return Ok(None);
    };
    let Some(description) = fields.get("description").cloned() else {
        eprintln!("[skills] skipping {}: missing description", path.display());
        return Ok(None);
    };
    if !valid_skill_name(&name) {
        eprintln!(
            "[skills] skipping {}: invalid name `{}`",
            path.display(),
            name
        );
        return Ok(None);
    }
    Ok(Some(Skill {
        name,
        description,
        path: path.to_path_buf(),
        dir: path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
    }))
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
    dir.join(".git").exists()
        || dir.join(".ferrum").exists()
        || dir.join("AGENTS.md").exists()
        || dir.join("agents.md").exists()
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_skill_names() {
        assert!(valid_skill_name("pdf-tools"));
        assert!(!valid_skill_name("PDF"));
        assert!(!valid_skill_name("-pdf"));
        assert!(!valid_skill_name("pdf--tools"));
    }

    #[test]
    fn parses_frontmatter() {
        let fm = extract_frontmatter("---\nname: test\ndescription: hello\n---\nbody").unwrap();
        let fields = parse_frontmatter_fields(&fm).unwrap();
        assert_eq!(fields.get("name").unwrap(), "test");
        assert_eq!(fields.get("description").unwrap(), "hello");
    }

    #[test]
    fn outside_git_repo_does_not_load_parent_tmp_agents_skills() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path();
        let cwd = parent.join("child");
        fs::create_dir_all(&cwd).unwrap();

        let ancestors = project_ancestors_with_boundary(&cwd, Some(parent));

        assert_eq!(ancestors, vec![cwd]);
    }

    #[test]
    fn project_ancestors_stop_at_marker() {
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
        fs::create_dir_all(skills_dir.join("a")).unwrap();
        fs::create_dir_all(skills_dir.join("b")).unwrap();
        fs::write(
            skills_dir.join("a/SKILL.md"),
            "---\nname: dup\ndescription: first\n---\nbody\n",
        )
        .unwrap();
        fs::write(
            skills_dir.join("b/SKILL.md"),
            "---\nname: dup\ndescription: second\n---\nbody\n",
        )
        .unwrap();
        let mut skills = Vec::new();

        let error = add_skills_from_dir(&mut skills, &skills_dir, true).unwrap_err();

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

        let skill = parse_skill_file(&path).unwrap().unwrap();

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

        let skill = parse_skill_file(&path).unwrap().unwrap();

        assert_eq!(skill.name, "crlf-skill");
    }

    #[test]
    fn skill_frontmatter_uses_yaml_colon_strings() {
        let fm = "name: yaml-skill\ndescription: \"does: parse: colons\"\n";
        let fields = parse_frontmatter_fields(fm).unwrap();
        assert_eq!(fields.get("description").unwrap(), "does: parse: colons");
    }
}

use anyhow::{Context, Result};
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

    if direct_md {
        for entry in
            fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_file()
                && path.extension().and_then(|ext| ext.to_str()) == Some("md")
                && let Some(skill) = parse_skill_file(&path)?
            {
                skills.push(skill);
            }
        }
    }

    visit_skill_dirs(dir, skills)
}

fn visit_skill_dirs(dir: &Path, skills: &mut Vec<Skill>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
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

fn parse_skill_file(path: &Path) -> Result<Option<Skill>> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let Some(frontmatter) = extract_frontmatter(&text) else {
        eprintln!("[skills] skipping {}: missing frontmatter", path.display());
        return Ok(None);
    };
    let fields = parse_frontmatter_fields(frontmatter);
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

fn extract_frontmatter(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

fn strip_frontmatter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---\n") else {
        return text;
    };
    let Some(end) = rest.find("\n---") else {
        return text;
    };
    &rest[end + "\n---".len()..]
}

fn parse_frontmatter_fields(frontmatter: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    for line in frontmatter.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches(['\'', '"']).to_string();
        fields.insert(key.trim().to_string(), value);
    }
    fields
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
    let mut dirs = Vec::new();
    let mut current = Some(cwd);
    while let Some(dir) = current {
        dirs.push(dir.to_path_buf());
        if dir.join(".git").exists() {
            break;
        }
        current = dir.parent();
    }
    dirs
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
        let fields = parse_frontmatter_fields(fm);
        assert_eq!(fields.get("name").unwrap(), "test");
        assert_eq!(fields.get("description").unwrap(), "hello");
    }
}

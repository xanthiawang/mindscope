use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::types::*;

/// Extract YAML frontmatter from markdown content
fn parse_frontmatter(content: &str) -> (HashMap<String, String>, String) {
    let mut meta = HashMap::new();
    let body;

    if content.starts_with("---") {
        if let Some(end) = content[3..].find("---") {
            let yaml_str = &content[3..3 + end];
            for line in yaml_str.lines() {
                if let Some((key, val)) = line.split_once(':') {
                    let k = key.trim().to_string();
                    let v = val.trim().trim_matches('\'').trim_matches('"').to_string();
                    meta.insert(k, v);
                }
            }
            body = content[3 + end + 3..].trim().to_string();
        } else {
            body = content.to_string();
        }
    } else {
        body = content.to_string();
    }

    (meta, body)
}

/// Extract a field value from markdown body like "**Last contact**: 2026-03-14"
fn extract_field(body: &str, field: &str) -> Option<String> {
    for line in body.lines() {
        let lower = line.to_lowercase();
        if lower.contains(&field.to_lowercase()) {
            if let Some(pos) = line.find(':') {
                let val = line[pos + 1..].trim().to_string();
                if !val.is_empty() {
                    return Some(val);
                }
            }
        }
    }
    None
}

/// Extract checkbox items from markdown
fn extract_action_items(body: &str) -> Vec<ActionItem> {
    body.lines()
        .filter(|line| line.contains("- [") || line.contains("- [x]"))
        .map(|line| {
            let done = line.contains("[x]") || line.contains("[X]");
            let text = line
                .trim_start_matches(|c: char| c == '-' || c == ' ' || c == '[' || c == 'x' || c == 'X' || c == ']')
                .trim()
                .to_string();

            // Try to extract assignee from [[user.name]] pattern
            let assignee = if let Some(start) = text.find("[[user.") {
                let rest = &text[start + 7..];
                rest.find("]]").map(|end| rest[..end].to_string())
            } else {
                None
            };

            ActionItem {
                text,
                assignee,
                done,
                due_date: None,
            }
        })
        .collect()
}

/// Parse all user.*.md files from vault directory
pub fn parse_people(vault_path: &Path) -> Vec<Person> {
    let pattern = vault_path.join("user.*.md").to_string_lossy().to_string();
    let mut people = Vec::new();

    for entry in glob::glob(&pattern).unwrap_or_else(|_| glob::glob("").unwrap()) {
        if let Ok(path) = entry {
            let filename = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
            // Skip the base user.md file
            if filename == "user" {
                continue;
            }

            if let Ok(content) = fs::read_to_string(&path) {
                let (meta, body) = parse_frontmatter(&content);
                let name = meta.get("title").cloned().unwrap_or_else(|| {
                    filename.strip_prefix("user.").unwrap_or(&filename).replace('-', " ")
                });

                people.push(Person {
                    filename,
                    name,
                    role: meta.get("desc").cloned().unwrap_or_default(),
                    email: extract_field(&body, "email"),
                    last_contact: extract_field(&body, "last contact"),
                    updated: meta.get("updated").and_then(|v| v.parse().ok()).unwrap_or(0),
                });
            }
        }
    }

    people.sort_by(|a, b| b.updated.cmp(&a.updated));
    people
}

/// Parse a single person detail
pub fn parse_person_detail(vault_path: &Path, name: &str) -> Option<PersonDetail> {
    let file_path = vault_path.join(format!("user.{}.md", name));
    let content = fs::read_to_string(&file_path).ok()?;
    let (meta, body) = parse_frontmatter(&content);

    let person = Person {
        filename: format!("user.{}", name),
        name: meta.get("title").cloned().unwrap_or_else(|| name.replace('-', " ")),
        role: meta.get("desc").cloned().unwrap_or_default(),
        email: extract_field(&body, "email"),
        last_contact: extract_field(&body, "last contact"),
        updated: meta.get("updated").and_then(|v| v.parse().ok()).unwrap_or(0),
    };

    // Find related meetings and projects by grep-ing for the person's name
    let related_meetings = find_references(vault_path, "meet.*.md", name);
    let related_projects = find_references(vault_path, "proj.*.md", name);

    Some(PersonDetail {
        person,
        content: body,
        related_meetings,
        related_projects,
    })
}

/// Parse all meet.*.md files
pub fn parse_meetings(vault_path: &Path) -> Vec<Meeting> {
    let pattern = vault_path.join("meet.*.md").to_string_lossy().to_string();
    let mut meetings = Vec::new();

    for entry in glob::glob(&pattern).unwrap_or_else(|_| glob::glob("").unwrap()) {
        if let Ok(path) = entry {
            let filename = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
            if let Ok(content) = fs::read_to_string(&path) {
                let (meta, body) = parse_frontmatter(&content);

                // Extract date from filename: meet.2026.03.14 -> 2026-03-14
                let date = filename
                    .strip_prefix("meet.")
                    .unwrap_or(&filename)
                    .replace('.', "-");

                // Extract attendees from frontmatter or body
                let attendees = extract_attendees(&meta, &body);

                meetings.push(Meeting {
                    filename,
                    date,
                    title: meta.get("title").cloned().unwrap_or_else(|| "Meeting".to_string()),
                    attendees,
                    updated: meta.get("updated").and_then(|v| v.parse().ok()).unwrap_or(0),
                });
            }
        }
    }

    meetings.sort_by(|a, b| b.date.cmp(&a.date));
    meetings
}

/// Parse a single meeting detail
pub fn parse_meeting_detail(vault_path: &Path, date: &str) -> Option<MeetingDetail> {
    let date_dots = date.replace('-', ".");
    let file_path = vault_path.join(format!("meet.{}.md", date_dots));
    let content = fs::read_to_string(&file_path).ok()?;
    let (meta, body) = parse_frontmatter(&content);

    let attendees = extract_attendees(&meta, &body);
    let action_items = extract_action_items(&body);

    let meeting = Meeting {
        filename: format!("meet.{}", date_dots),
        date: date.to_string(),
        title: meta.get("title").cloned().unwrap_or_else(|| "Meeting".to_string()),
        attendees,
        updated: meta.get("updated").and_then(|v| v.parse().ok()).unwrap_or(0),
    };

    Some(MeetingDetail {
        meeting,
        content: body,
        action_items,
        audio_transcript: None,
    })
}

/// Parse all proj.*.md files
pub fn parse_projects(vault_path: &Path) -> Vec<Project> {
    let pattern = vault_path.join("proj.*.md").to_string_lossy().to_string();
    let mut projects = Vec::new();

    for entry in glob::glob(&pattern).unwrap_or_else(|_| glob::glob("").unwrap()) {
        if let Ok(path) = entry {
            let filename = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
            if let Ok(content) = fs::read_to_string(&path) {
                let (meta, body) = parse_frontmatter(&content);

                let name = meta.get("title").cloned().unwrap_or_else(|| {
                    filename.strip_prefix("proj.").unwrap_or(&filename).to_string()
                });

                let momentum = extract_field(&body, "momentum").unwrap_or_else(|| "UNKNOWN".to_string());

                projects.push(Project {
                    filename,
                    name,
                    status: meta.get("status").cloned().unwrap_or_else(|| "unknown".to_string()),
                    momentum,
                    updated: meta.get("updated").and_then(|v| v.parse().ok()).unwrap_or(0),
                });
            }
        }
    }

    projects.sort_by(|a, b| b.updated.cmp(&a.updated));
    projects
}

/// Extract attendees from frontmatter or body
fn extract_attendees(meta: &HashMap<String, String>, body: &str) -> Vec<String> {
    // Check frontmatter attendees field
    if let Some(att) = meta.get("attendees") {
        return att
            .split(',')
            .map(|s| {
                s.trim()
                    .trim_matches(|c: char| c == '[' || c == ']' || c == '\'' || c == '"')
                    .to_string()
            })
            .filter(|s| !s.is_empty())
            .collect();
    }

    // Extract wikilinks from body that reference user.*
    body.lines()
        .flat_map(|line| {
            let mut names = Vec::new();
            let mut search = line;
            while let Some(start) = search.find("[[") {
                if let Some(end) = search[start..].find("]]") {
                    let link = &search[start + 2..start + end];
                    let name = if let Some((_display, target)) = link.split_once('|') {
                        target.strip_prefix("user.").unwrap_or(target)
                    } else {
                        link.strip_prefix("user.").unwrap_or(link)
                    };
                    names.push(name.replace('-', " "));
                    search = &search[start + end + 2..];
                } else {
                    break;
                }
            }
            names
        })
        .collect::<Vec<_>>()
        .into_iter()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
}

/// Find files matching pattern that contain a reference to target
fn find_references(vault_path: &Path, pattern: &str, target: &str) -> Vec<String> {
    let full_pattern = vault_path.join(pattern).to_string_lossy().to_string();
    let mut refs = Vec::new();

    for entry in glob::glob(&full_pattern).unwrap_or_else(|_| glob::glob("").unwrap()) {
        if let Ok(path) = entry {
            if let Ok(content) = fs::read_to_string(&path) {
                if content.to_lowercase().contains(&target.to_lowercase()) {
                    refs.push(path.file_stem().unwrap_or_default().to_string_lossy().to_string());
                }
            }
        }
    }

    refs
}

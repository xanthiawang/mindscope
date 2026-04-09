use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Person {
    pub filename: String,
    pub name: String,
    pub role: String,
    pub email: Option<String>,
    pub last_contact: Option<String>,
    pub updated: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonDetail {
    #[serde(flatten)]
    pub person: Person,
    pub content: String,
    pub related_meetings: Vec<String>,
    pub related_projects: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meeting {
    pub filename: String,
    pub date: String,
    pub title: String,
    pub attendees: Vec<String>,
    pub updated: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingDetail {
    #[serde(flatten)]
    pub meeting: Meeting,
    pub content: String,
    pub action_items: Vec<ActionItem>,
    pub audio_transcript: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionItem {
    pub text: String,
    pub assignee: Option<String>,
    pub done: bool,
    pub due_date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub filename: String,
    pub name: String,
    pub status: String,
    pub momentum: String,
    pub updated: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DateRange {
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingSummary {
    pub summary: String,
    pub key_decisions: Vec<String>,
    pub action_items: Vec<ActionItem>,
}

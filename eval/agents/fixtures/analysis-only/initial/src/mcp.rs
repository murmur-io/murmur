pub struct Note {
    pub content: String,
    pub unlocked: bool,
}

pub fn export_note(note: &Note) -> String {
    note.content.clone()
}

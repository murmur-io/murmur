#[derive(Clone, Debug)]
pub struct Note {
    pub content: String,
    pub audio_path: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NoteDto {
    pub content: Option<String>,
    pub audio_path: Option<String>,
}

pub fn to_dto(note: &Note, _unlocked: bool) -> NoteDto {
    NoteDto {
        content: Some(note.content.clone()),
        audio_path: note.audio_path.clone(),
    }
}

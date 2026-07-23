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

pub fn to_dto(note: &Note, unlocked: bool) -> NoteDto {
    NoteDto {
        content: unlocked.then(|| note.content.clone()),
        audio_path: unlocked.then(|| note.audio_path.clone()).flatten(),
    }
}

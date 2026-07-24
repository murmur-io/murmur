#[derive(Clone, Debug)]
pub struct SealRecord {
    pub plaintext: Option<Vec<u8>>,
    pub sealed: Vec<u8>,
}

pub fn seal(record: &mut SealRecord, verify_ok: bool) -> Result<(), &'static str> {
    let plaintext = record.plaintext.clone().ok_or("missing plaintext")?;
    let candidate = plaintext.clone();
    if !verify_ok || candidate != plaintext {
        return Err("seal verification failed");
    }
    record.sealed = candidate;
    record.plaintext = None;
    Ok(())
}

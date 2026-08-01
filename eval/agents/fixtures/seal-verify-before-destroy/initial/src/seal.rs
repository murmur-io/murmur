#[derive(Clone, Debug)]
pub struct SealRecord {
    pub plaintext: Option<Vec<u8>>,
    pub sealed: Vec<u8>,
}

pub fn seal(record: &mut SealRecord, verify_ok: bool) -> Result<(), &'static str> {
    let plaintext = record.plaintext.clone().ok_or("missing plaintext")?;
    record.sealed = plaintext;
    record.plaintext = None;
    if !verify_ok {
        return Err("seal verification failed");
    }
    Ok(())
}

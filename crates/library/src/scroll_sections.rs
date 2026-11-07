/// A letter-index bucket and the row where it starts.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct ScrollSection {
    pub title: String,
    pub index: u64,
}

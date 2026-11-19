pub use crate::database::GameFile;

pub fn visible<'a>(rows: &'a [GameFile], query: &'a str) -> impl Iterator<Item = &'a GameFile> {
    let query = query.to_lowercase();
    rows.iter().filter(move |row| {
        query.is_empty()
            || row.title_name.to_lowercase().contains(&query)
            || row.filename.to_lowercase().contains(&query)
            || row.title_id.to_lowercase().contains(&query)
    })
}

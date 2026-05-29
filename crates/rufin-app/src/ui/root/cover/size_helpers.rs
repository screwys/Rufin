pub(in crate::ui) fn cover_size_from_cache_key(key: &str) -> Option<i32> {
    key.rsplit('/')
        .next()
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|size| *size > 0)
}

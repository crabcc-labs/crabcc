/// Estimate the number of tokens in a given text.
///
/// This function approximates token count by dividing the text length by 4.
/// If the text is non-empty but the division results in 0, it returns 1.
/// Empty text returns 0.
pub fn estimate_tokens(text: &str) -> usize {
    let len = text.len();
    if len == 0 {
        0
    } else {
        let tokens = len / 4;
        if tokens == 0 {
            1
        } else {
            tokens
        }
    }
}

pub trait HasLocalChecksum {
    fn local_checksum(&self) -> &str;
    fn order(&self) -> i64;
    fn local_identifier(&self) -> &str;
}

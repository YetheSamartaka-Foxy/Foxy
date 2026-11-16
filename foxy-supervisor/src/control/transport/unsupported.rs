use super::{IoBudget, LocalEndpoint};
use std::{io, marker::PhantomData};

pub(super) struct Listener;
pub(super) struct Connection<'a>(PhantomData<&'a mut Listener>);

fn unavailable<T>() -> io::Result<T> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "local control unavailable",
    ))
}

impl Listener {
    pub(super) fn bind(_: &LocalEndpoint) -> io::Result<Self> {
        unavailable()
    }
    pub(super) fn accept(&mut self, _: IoBudget<'_>) -> io::Result<Connection<'_>> {
        unavailable()
    }
}

pub(super) fn connect<'a>(_: &LocalEndpoint, _: IoBudget<'_>) -> io::Result<Connection<'a>> {
    unavailable()
}

impl Connection<'_> {
    pub(super) fn read_exact(&mut self, _: &mut [u8], _: IoBudget<'_>) -> io::Result<()> {
        unavailable()
    }
    pub(super) fn write_all(&mut self, _: &[u8], _: IoBudget<'_>) -> io::Result<()> {
        unavailable()
    }
}

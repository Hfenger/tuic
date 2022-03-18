use super::{Address, Error};
use bytes::BufMut;
use tokio::io::{AsyncRead, AsyncReadExt};

/// UDP Associate header
///
/// ```plain
/// +-----+------+------+----------+----------+----------+
/// | RSV | FRAG | ATYP | DST.ADDR | DST.PORT |   DATA   |
/// +-----+------+------+----------+----------+----------+
/// |  2  |  1   |  1   | Variable |    2     | Variable |
/// +-----+------+------+----------+----------+----------+
/// ```
#[derive(Clone, Debug)]
pub struct UdpHeader {
    pub frag: u8,
    pub address: Address,
}

impl UdpHeader {
    pub fn new(frag: u8, address: Address) -> Self {
        Self { frag, address }
    }

    pub async fn read_from<R>(r: &mut R) -> Result<Self, Error>
    where
        R: AsyncRead + Unpin,
    {
        let mut buf = [0; 3];
        r.read_exact(&mut buf).await?;

        let frag = buf[2];

        let address = Address::read_from(r).await?;
        Ok(Self { frag, address })
    }

    pub fn write_to_buf<B: BufMut>(&self, buf: &mut B) {
        buf.put_bytes(0x00, 2);
        buf.put_u8(self.frag);
        self.address.write_to_buf(buf);
    }

    pub fn serialized_len(&self) -> usize {
        3 + self.address.serialized_len()
    }
}

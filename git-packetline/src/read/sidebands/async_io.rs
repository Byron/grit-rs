use crate::{
    decode,
    immutable::{Band, Text},
    PacketLine, StreamingPeekableIter, U16_HEX_BYTES,
};
use futures_io::{AsyncBufRead, AsyncRead};
use futures_lite::ready;
use std::future::Future;
use std::{
    pin::Pin,
    task::{Context, Poll},
};

type ReadLineResult<'a> = Option<std::io::Result<Result<PacketLine<'a>, decode::Error>>>;
/// An implementor of [`AsyncBufRead`] yielding packet lines on each call to [`read_line()`][AsyncBufRead::read_line()].
/// It's also possible to hide the underlying packet lines using the [`Read`][AsyncRead] implementation which is useful
/// if they represent binary data, like the one of a pack file.
#[pin_project::pin_project(PinnedDrop)]
pub struct WithSidebands<'a, T, F>
where
    T: AsyncRead,
{
    #[pin]
    parent: Option<&'a mut StreamingPeekableIter<T>>,
    handle_progress: Option<F>,
    read_line: Option<Pin<Box<dyn Future<Output = ReadLineResult<'a>> + 'a>>>,
    pos: usize,
    cap: usize,
}

#[pin_project::pinned_drop]
impl<'a, T, F> PinnedDrop for WithSidebands<'a, T, F>
where
    T: AsyncRead,
{
    fn drop(mut self: Pin<&mut Self>) {
        let mut this = self.project();
        this.parent.take().map(|p| p.reset());
    }
}

impl<'a, T> WithSidebands<'a, T, fn(bool, &[u8])>
where
    T: AsyncRead,
{
    /// Create a new instance with the given provider as `parent`.
    pub fn new(parent: &'a mut StreamingPeekableIter<T>) -> Self {
        WithSidebands {
            parent: Some(parent),
            handle_progress: None,
            read_line: None,
            pos: 0,
            cap: 0,
        }
    }
}

impl<'a, T, F> WithSidebands<'a, T, F>
where
    T: AsyncRead + Unpin,
    F: FnMut(bool, &[u8]),
{
    /// Create a new instance with the given `parent` provider and the `handle_progress` function.
    ///
    /// Progress or error information will be passed to the given `handle_progress(is_error, text)` function, with `is_error: bool`
    /// being true in case the `text` is to be interpreted as error.
    pub fn with_progress_handler(parent: &'a mut StreamingPeekableIter<T>, handle_progress: F) -> Self {
        WithSidebands {
            parent: Some(parent),
            handle_progress: Some(handle_progress),
            read_line: None,
            pos: 0,
            cap: 0,
        }
    }

    /// Create a new instance without a progress handler.
    pub fn without_progress_handler(parent: &'a mut StreamingPeekableIter<T>) -> Self {
        WithSidebands {
            parent: Some(parent),
            handle_progress: None,
            read_line: None,
            pos: 0,
            cap: 0,
        }
    }

    /// Forwards to the parent [StreamingPeekableIter::reset_with()]
    pub fn reset_with(&mut self, delimiters: &'static [PacketLine<'static>]) {
        self.parent.as_mut().unwrap().reset_with(delimiters)
    }

    /// Forwards to the parent [StreamingPeekableIter::stopped_at()]
    pub fn stopped_at(&self) -> Option<PacketLine<'static>> {
        self.parent.as_ref().unwrap().stopped_at
    }

    /// Set or unset the progress handler.
    pub fn set_progress_handler(&mut self, handle_progress: Option<F>) {
        self.handle_progress = handle_progress;
    }

    /// Effectively forwards to the parent [StreamingPeekableIter::peek_line()], allowing to see what would be returned
    /// next on a call to [`read_line()`][io::BufRead::read_line()].
    pub async fn peek_data_line(&mut self) -> Option<std::io::Result<Result<&[u8], crate::decode::Error>>> {
        match self.parent.as_mut().unwrap().peek_line().await {
            Some(Ok(Ok(crate::PacketLine::Data(line)))) => Some(Ok(Ok(line))),
            Some(Ok(Err(err))) => Some(Ok(Err(err))),
            Some(Err(err)) => Some(Err(err)),
            _ => None,
        }
    }
}

impl<'a, T, F> AsyncBufRead for WithSidebands<'a, T, F>
where
    T: AsyncRead + Unpin + Send,
    F: FnMut(bool, &[u8]),
{
    fn poll_fill_buf(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<&[u8]>> {
        use futures_lite::FutureExt;
        use std::io;
        {
            let this = self.as_mut().get_mut();
            if this.pos >= this.cap {
                let (ofs, cap) = loop {
                    // todo!("poll a future based on a field of ourselves - self-ref once again");
                    this.read_line = Some(this.parent.take().unwrap().read_line().boxed());
                    let line = match ready!(this.read_line.as_mut().expect("set above").poll(_cx)) {
                        Some(line) => line?.map_err(|err| io::Error::new(io::ErrorKind::Other, err))?,
                        None => break (0, 0),
                    };
                    match this.handle_progress.as_mut() {
                        Some(handle_progress) => {
                            let band = line
                                .decode_band()
                                .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
                            const ENCODED_BAND: usize = 1;
                            match band {
                                Band::Data(d) => break (U16_HEX_BYTES + ENCODED_BAND, d.len()),
                                Band::Progress(d) => {
                                    let text = Text::from(d).0;
                                    handle_progress(false, text);
                                }
                                Band::Error(d) => {
                                    let text = Text::from(d).0;
                                    handle_progress(true, text);
                                }
                            };
                        }
                        None => {
                            break match line.as_slice() {
                                Some(d) => (U16_HEX_BYTES, d.len()),
                                None => {
                                    return Poll::Ready(Err(io::Error::new(
                                        io::ErrorKind::UnexpectedEof,
                                        "encountered non-data line in a data-line only context",
                                    )))
                                }
                            }
                        }
                    }
                };
                this.cap = cap + ofs;
                this.pos = ofs;
            }
        }
        let range = self.pos..self.cap;
        Poll::Ready(Ok(&self.get_mut().parent.as_ref().unwrap().buf[range]))
    }

    fn consume(self: Pin<&mut Self>, amt: usize) {
        let this = self.project();
        *this.pos = std::cmp::min(*this.pos + amt, *this.cap);
    }
}

impl<'a, T, F> AsyncRead for WithSidebands<'a, T, F>
where
    T: AsyncRead + Unpin + Send,
    F: FnMut(bool, &[u8]),
{
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<std::io::Result<usize>> {
        let nread = {
            use std::io::Read;
            let mut rem = ready!(self.as_mut().poll_fill_buf(cx))?;
            rem.read(buf)?
        };
        self.consume(nread);
        Poll::Ready(Ok(nread))
    }
}
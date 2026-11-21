use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::{Event, xfixes, xproto};
use x11rb::rust_connection::RustConnection;

type Failure = Box<dyn std::error::Error + Send + Sync>;

pub const MAX_TEXT: usize = 200_000;
const COPY_WINDOW: Duration = Duration::from_secs(2);

pub struct Clipboard {
    window: xproto::Window,
    selection: xproto::Atom,
    utf8: xproto::Atom,
    targets: xproto::Atom,
    plain: xproto::Atom,
    property: xproto::Atom,
    offered: Option<Vec<u8>>,
    copied: Option<Vec<u8>>,
    wanted_until: Option<Instant>,
}

fn atom(conn: &RustConnection, name: &str) -> Result<xproto::Atom, Failure> {
    Ok(xproto::intern_atom(conn, false, name.as_bytes())?
        .reply()?
        .atom)
}

impl Clipboard {
    pub fn new(conn: &RustConnection, root: xproto::Window) -> Result<Self, Failure> {
        let window = conn.generate_id()?;
        xproto::create_window(
            conn,
            0,
            window,
            root,
            -1,
            -1,
            1,
            1,
            0,
            xproto::WindowClass::INPUT_ONLY,
            0,
            &xproto::CreateWindowAux::new(),
        )?
        .check()?;
        let selection = atom(conn, "CLIPBOARD")?;
        xfixes::select_selection_input(
            conn,
            window,
            selection,
            xfixes::SelectionEventMask::SET_SELECTION_OWNER,
        )?
        .check()?;
        Ok(Self {
            window,
            selection,
            utf8: atom(conn, "UTF8_STRING")?,
            targets: atom(conn, "TARGETS")?,
            plain: atom(conn, "text/plain;charset=utf-8")?,
            property: atom(conn, "KUMOBA_VIEW_CLIPBOARD")?,
            offered: None,
            copied: None,
            wanted_until: None,
        })
    }

    pub fn offer(&mut self, conn: &RustConnection, text: &[u8]) -> Result<(), Failure> {
        let mut end = text.len().min(MAX_TEXT);
        while end > 0 && end < text.len() && text[end] & 0xc0 == 0x80 {
            end -= 1;
        }
        self.offered = Some(text[..end].to_vec());
        xproto::set_selection_owner(conn, self.window, self.selection, x11rb::CURRENT_TIME)?;
        Ok(())
    }

    pub fn expect_copy(&mut self) {
        self.wanted_until = Some(Instant::now() + COPY_WINDOW);
    }

    fn wanted(&self) -> bool {
        self.wanted_until
            .is_some_and(|until| Instant::now() < until)
    }

    pub fn copied(&mut self) -> Option<Vec<u8>> {
        self.copied.take()
    }

    fn is_text(&self, target: xproto::Atom) -> bool {
        target == self.utf8
            || target == self.plain
            || target == xproto::Atom::from(xproto::AtomEnum::STRING)
    }

    pub fn handle(&mut self, conn: &RustConnection, event: &Event) -> Result<(), Failure> {
        match event {
            Event::SelectionRequest(request) if request.owner == self.window => {
                let property = if request.property == x11rb::NONE {
                    request.target
                } else {
                    request.property
                };
                let mut answered = x11rb::NONE;
                if request.target == self.targets {
                    let list = [
                        self.targets,
                        self.utf8,
                        self.plain,
                        xproto::Atom::from(xproto::AtomEnum::STRING),
                    ];
                    let bytes: Vec<u8> = list.iter().flat_map(|atom| atom.to_ne_bytes()).collect();
                    xproto::change_property(
                        conn,
                        xproto::PropMode::REPLACE,
                        request.requestor,
                        property,
                        xproto::AtomEnum::ATOM,
                        32,
                        list.len() as u32,
                        &bytes,
                    )?;
                    answered = property;
                } else if let Some(text) = self
                    .offered
                    .as_ref()
                    .filter(|_| self.is_text(request.target))
                {
                    xproto::change_property(
                        conn,
                        xproto::PropMode::REPLACE,
                        request.requestor,
                        property,
                        request.target,
                        8,
                        text.len() as u32,
                        text,
                    )?;
                    answered = property;
                }
                let notify = xproto::SelectionNotifyEvent {
                    response_type: xproto::SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: request.time,
                    requestor: request.requestor,
                    selection: request.selection,
                    target: request.target,
                    property: answered,
                };
                xproto::send_event(
                    conn,
                    false,
                    request.requestor,
                    xproto::EventMask::NO_EVENT,
                    notify,
                )?;
                conn.flush()?;
            }
            Event::SelectionClear(clear) if clear.owner == self.window => self.offered = None,
            Event::XfixesSelectionNotify(change)
                if change.selection == self.selection
                    && change.owner != self.window
                    && change.owner != x11rb::NONE
                    && self.wanted() =>
            {
                xproto::convert_selection(
                    conn,
                    self.window,
                    self.selection,
                    self.utf8,
                    self.property,
                    change.selection_timestamp,
                )?;
                conn.flush()?;
            }
            Event::SelectionNotify(done)
                if done.requestor == self.window && done.property == self.property =>
            {
                let reply = xproto::get_property(
                    conn,
                    true,
                    self.window,
                    self.property,
                    xproto::AtomEnum::ANY,
                    0,
                    (MAX_TEXT / 4) as u32,
                )?
                .reply()?;
                if reply.format == 8
                    && reply.bytes_after == 0
                    && self.is_text(reply.type_)
                    && self.wanted()
                {
                    self.copied = Some(reply.value);
                    self.wanted_until = None;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

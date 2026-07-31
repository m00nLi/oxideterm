// Copyright (C) 2026 AnalyseDeCircuit
// SPDX-License-Identifier: GPL-3.0-only

use std::{io::Write, sync::mpsc};

use crate::{RemoteDesktopHelperRequest, RemoteDesktopJsonLineError, write_request_line};

const REQUEST_DRAIN_LIMIT: usize = 128;

#[derive(Default)]
pub struct RemoteDesktopRequestWriteCoalescer {
    pending_mouse_move: Option<RemoteDesktopHelperRequest>,
}

impl RemoteDesktopRequestWriteCoalescer {
    pub fn push(
        &mut self,
        request: RemoteDesktopHelperRequest,
        output: &mut Vec<RemoteDesktopHelperRequest>,
    ) {
        match request {
            RemoteDesktopHelperRequest::MouseMove { .. } => {
                // Mouse movement is lossy state, while key and button edges are ordered.
                self.pending_mouse_move = Some(request);
            }
            request => {
                self.flush(output);
                output.push(request);
            }
        }
    }

    pub fn flush(&mut self, output: &mut Vec<RemoteDesktopHelperRequest>) {
        if let Some(request) = self.pending_mouse_move.take() {
            output.push(request);
        }
    }
}

pub fn write_remote_desktop_requests(
    writer: &mut impl Write,
    request_rx: mpsc::Receiver<RemoteDesktopHelperRequest>,
) -> Result<(), RemoteDesktopJsonLineError> {
    loop {
        let Ok(first_request) = request_rx.recv() else {
            return Ok(());
        };
        let mut disconnected = false;
        let mut coalescer = RemoteDesktopRequestWriteCoalescer::default();
        let mut requests = Vec::new();
        coalescer.push(first_request, &mut requests);

        for _ in 0..REQUEST_DRAIN_LIMIT {
            match request_rx.try_recv() {
                Ok(request) => coalescer.push(request, &mut requests),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        coalescer.flush(&mut requests);

        for request in requests {
            let should_close = matches!(request, RemoteDesktopHelperRequest::Close);
            write_request_line(writer, &request)?;
            if should_close {
                return Ok(());
            }
        }

        if disconnected {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::{RemoteDesktopMouseButton, RemoteDesktopMouseButtonState, read_request_line};

    #[test]
    fn writer_coalesces_mouse_moves_without_reordering_button_edges() {
        let (request_tx, request_rx) = mpsc::channel();
        request_tx
            .send(RemoteDesktopHelperRequest::MouseMove { x: 10, y: 20 })
            .unwrap();
        request_tx
            .send(RemoteDesktopHelperRequest::MouseMove { x: 30, y: 40 })
            .unwrap();
        request_tx
            .send(RemoteDesktopHelperRequest::MouseButton {
                button: RemoteDesktopMouseButton::Left,
                state: RemoteDesktopMouseButtonState::Pressed,
            })
            .unwrap();
        request_tx
            .send(RemoteDesktopHelperRequest::MouseMove { x: 50, y: 60 })
            .unwrap();
        drop(request_tx);

        let mut output = Vec::new();
        write_remote_desktop_requests(&mut output, request_rx).unwrap();

        let mut reader = Cursor::new(output);
        let mut decoded = Vec::new();
        while let Some(request) = read_request_line(&mut reader).unwrap() {
            decoded.push(request);
        }
        assert_eq!(
            decoded,
            vec![
                RemoteDesktopHelperRequest::MouseMove { x: 30, y: 40 },
                RemoteDesktopHelperRequest::MouseButton {
                    button: RemoteDesktopMouseButton::Left,
                    state: RemoteDesktopMouseButtonState::Pressed,
                },
                RemoteDesktopHelperRequest::MouseMove { x: 50, y: 60 },
            ]
        );
    }
}

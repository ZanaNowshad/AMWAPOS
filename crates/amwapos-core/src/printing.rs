//! Print queue and printer backends.
//!
//! Print jobs are inserted in the same transaction as the business record,
//! then executed after commit. A failed print leaves the job `failed` for
//! retry; it never affects the sale/refund.
//!
//! Backends:
//! * `network` — raw ESC/POS over TCP (usually port 9100)
//! * `windows` — raw ESC/POS through the Windows spooler (RAW datatype)
//! * `file`    — appends text renderings to a file (testing / virtual printer)
//! * `none`    — printing disabled
//!
//! Known limitation: ESC/POS text uses code page 437; Arabic text is not yet
//! rasterized and prints as '?'. Receipts are built from English snapshots.

use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::ids::new_id;
use crate::receipt::{self, ReceiptDoc};
use crate::service::AppCore;
use crate::settings::{self, PrinterSettings};
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrintOutcome {
    /// printed | failed | queued | disabled
    pub status: String,
    pub message: Option<String>,
    pub job_id: Option<String>,
}

impl PrintOutcome {
    pub fn queued() -> Self {
        Self { status: "queued".into(), message: None, job_id: None }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PrintJobRow {
    pub job_id: String,
    pub kind: String,
    pub ref_id: Option<String>,
    pub reference: Option<String>,
    pub status: String,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub created_at: String,
}

pub fn enqueue(c: &Connection, kind: &str, ref_id: &str, copy_label: Option<&str>, user: Option<&str>) -> AppResult<String> {
    let id = new_id();
    let now = time::now_str();
    c.execute(
        "INSERT INTO print_jobs(job_id, kind, ref_type, ref_id, copy_label, status, attempts, created_by, created_at, updated_at)
         VALUES (?1,?2,?2,?3,?4,'pending',0,?5,?6,?6)",
        params![id, kind, ref_id, copy_label, user, now],
    )?;
    Ok(id)
}

pub fn enqueue_drawer_pulse(c: &Connection, user: Option<&str>, ref_id: &str) -> AppResult<Option<String>> {
    let p: PrinterSettings = settings::get(c, settings::KEY_PRINTER)?;
    if !p.drawer_pulse || p.mode == "none" {
        return Ok(None);
    }
    Ok(Some(enqueue(c, "drawer", ref_id, None, user)?))
}

pub fn latest_job_outcome(c: &Connection, kind: &str, ref_id: &str) -> AppResult<Option<PrintOutcome>> {
    Ok(c.query_row(
        "SELECT job_id, status, last_error FROM print_jobs WHERE kind=?1 AND ref_id=?2 ORDER BY created_at DESC LIMIT 1",
        params![kind, ref_id],
        |r| {
            let status: String = r.get(1)?;
            let err: Option<String> = r.get(2)?;
            Ok(PrintOutcome {
                status: match status.as_str() {
                    "pending" => "queued".into(),
                    "cancelled" => "disabled".into(),
                    other => other.to_string(),
                },
                message: err,
                job_id: Some(r.get(0)?),
            })
        },
    )
    .optional()?)
}

/// Render a job's document from committed records.
pub fn render_job(c: &Connection, kind: &str, ref_id: &str, copy: Option<&str>) -> AppResult<Option<ReceiptDoc>> {
    Ok(match kind {
        "sale" => Some(receipt::sale_receipt(c, ref_id, copy)?),
        "refund" => Some(receipt::refund_receipt(c, ref_id, copy)?),
        "shift_report" => Some(receipt::shift_report(c, ref_id)?),
        "test" => {
            let mut d = ReceiptDoc { width_chars: 48, blocks: vec![] };
            d.blocks.push(receipt::Block::Text {
                text: "AMWAPOS TEST PRINT".into(),
                align: receipt::Align::Center,
                bold: true,
                large: true,
            });
            d.blocks.push(receipt::Block::Text { text: time::now_str(), align: receipt::Align::Center, bold: false, large: false });
            d.blocks.push(receipt::Block::Rule);
            d.blocks.push(receipt::Block::Pair { left: "Printer".into(), right: "OK".into(), bold: false, large: false });
            Some(d)
        }
        _ => None,
    })
}

/// Deliver bytes to the configured printer.
pub fn send(p: &PrinterSettings, bytes: &[u8], text: &str) -> Result<(), String> {
    match p.mode.as_str() {
        "none" => Err("No receipt printer is configured.".into()),
        "network" => {
            let target = if p.target.contains(':') { p.target.clone() } else { format!("{}:9100", p.target) };
            let addr = target
                .to_socket_addrs()
                .map_err(|e| format!("Printer address {target} is invalid: {e}"))?
                .next()
                .ok_or_else(|| format!("Printer address {target} could not be resolved."))?;
            let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(3))
                .map_err(|e| format!("Printer at {target} is not reachable: {e}"))?;
            s.set_write_timeout(Some(Duration::from_secs(5))).ok();
            s.write_all(bytes).map_err(|e| format!("Sending to the printer failed: {e}"))?;
            s.flush().map_err(|e| format!("Sending to the printer failed: {e}"))?;
            Ok(())
        }
        "file" => {
            if p.target.trim().is_empty() {
                return Err("The file printer has no output path.".into());
            }
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p.target)
                .map_err(|e| format!("Cannot open {}: {e}", p.target))?;
            f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
            f.write_all(b"\n=====\n").map_err(|e| e.to_string())?;
            Ok(())
        }
        "windows" => windows_raw::print_raw(&p.target, bytes),
        other => Err(format!("Unknown printer mode '{other}'.")),
    }
}

#[cfg(windows)]
mod windows_raw {
    use windows_sys::Win32::Graphics::Printing::{
        ClosePrinter, EndDocPrinter, EndPagePrinter, OpenPrinterW, StartDocPrinterW, StartPagePrinter, WritePrinter, DOC_INFO_1W,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn print_raw(printer: &str, bytes: &[u8]) -> Result<(), String> {
        if printer.trim().is_empty() {
            return Err("Choose a Windows printer in Printer Settings.".into());
        }
        unsafe {
            let name = wide(printer);
            let mut h = std::ptr::null_mut();
            if OpenPrinterW(name.as_ptr(), &mut h, std::ptr::null()) == 0 {
                return Err(format!("Windows printer '{printer}' could not be opened."));
            }
            let mut doc_name = wide("AMWAPOS Receipt");
            let mut datatype = wide("RAW");
            let di = DOC_INFO_1W { pDocName: doc_name.as_mut_ptr(), pOutputFile: std::ptr::null_mut(), pDatatype: datatype.as_mut_ptr() };
            let job = StartDocPrinterW(h, 1, &di as *const DOC_INFO_1W as *const _);
            if job == 0 {
                ClosePrinter(h);
                return Err(format!("Windows printer '{printer}' refused the print job."));
            }
            StartPagePrinter(h);
            let mut written: u32 = 0;
            let ok = WritePrinter(h, bytes.as_ptr() as *const _, bytes.len() as u32, &mut written);
            EndPagePrinter(h);
            EndDocPrinter(h);
            ClosePrinter(h);
            if ok == 0 || written as usize != bytes.len() {
                return Err(format!("Windows printer '{printer}' did not accept all data."));
            }
            Ok(())
        }
    }

    pub fn list_printers() -> Vec<String> {
        use windows_sys::Win32::Graphics::Printing::{EnumPrintersW, PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL, PRINTER_INFO_4W};
        unsafe {
            let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
            let mut needed = 0u32;
            let mut count = 0u32;
            EnumPrintersW(flags, std::ptr::null(), 4, std::ptr::null_mut(), 0, &mut needed, &mut count);
            if needed == 0 {
                return vec![];
            }
            let mut buf = vec![0u8; needed as usize];
            if EnumPrintersW(flags, std::ptr::null(), 4, buf.as_mut_ptr(), needed, &mut needed, &mut count) == 0 {
                return vec![];
            }
            let infos = std::slice::from_raw_parts(buf.as_ptr() as *const PRINTER_INFO_4W, count as usize);
            infos
                .iter()
                .filter_map(|i| {
                    if i.pPrinterName.is_null() {
                        return None;
                    }
                    let mut len = 0;
                    while *i.pPrinterName.add(len) != 0 {
                        len += 1;
                    }
                    Some(String::from_utf16_lossy(std::slice::from_raw_parts(i.pPrinterName, len)))
                })
                .collect()
        }
    }
}

#[cfg(not(windows))]
mod windows_raw {
    pub fn print_raw(_printer: &str, _bytes: &[u8]) -> Result<(), String> {
        Err("Windows spooler printing is only available on Windows.".into())
    }
    pub fn list_printers() -> Vec<String> {
        vec![]
    }
}

pub fn system_printers() -> Vec<String> {
    windows_raw::list_printers()
}

impl AppCore {
    /// Execute one job. Never returns an error: the outcome describes it.
    pub fn print_job_run(&self, job_id: &str) -> PrintOutcome {
        let res: AppResult<PrintOutcome> = (|| {
            let (kind, ref_id, copy, status): (String, Option<String>, Option<String>, String) = self.db.read(|c| {
                c.query_row("SELECT kind, ref_id, copy_label, status FROM print_jobs WHERE job_id=?1", [job_id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .optional()?
                .ok_or_else(|| AppError::not_found("Print job"))
            })?;
            if status == "printed" {
                return Ok(PrintOutcome { status: "printed".into(), message: None, job_id: Some(job_id.into()) });
            }
            let p: PrinterSettings = self.db.read(|c| settings::get(c, settings::KEY_PRINTER))?;
            if p.mode == "none" {
                self.db.write(|tx| {
                    tx.execute(
                        "UPDATE print_jobs SET status='cancelled', last_error='No receipt printer is configured.', updated_at=?2 WHERE job_id=?1",
                        params![job_id, time::now_str()],
                    )?;
                    Ok(())
                })?;
                return Ok(PrintOutcome {
                    status: "disabled".into(),
                    message: Some("No receipt printer is configured.".into()),
                    job_id: Some(job_id.into()),
                });
            }
            let (bytes, text) = if kind == "drawer" {
                (receipt::drawer_pulse_bytes(), String::from("[drawer pulse]"))
            } else {
                let doc = self
                    .db
                    .read(|c| render_job(c, &kind, ref_id.as_deref().unwrap_or(""), copy.as_deref()))?
                    .ok_or_else(|| AppError::validation("Unknown print job type."))?;
                (doc.to_escpos(p.cut), doc.to_text())
            };
            let outcome = send(&p, &bytes, &text);
            let now = time::now_str();
            self.db.write(|tx| {
                match &outcome {
                    Ok(()) => tx.execute(
                        "UPDATE print_jobs SET status='printed', attempts=attempts+1, last_error=NULL, updated_at=?2 WHERE job_id=?1",
                        params![job_id, now],
                    )?,
                    Err(e) => tx.execute(
                        "UPDATE print_jobs SET status='failed', attempts=attempts+1, last_error=?3, updated_at=?2 WHERE job_id=?1",
                        params![job_id, now, e],
                    )?,
                };
                Ok(())
            })?;
            Ok(match outcome {
                Ok(()) => PrintOutcome { status: "printed".into(), message: None, job_id: Some(job_id.into()) },
                Err(e) => {
                    tracing::warn!(job_id, error = %e, "print failed");
                    PrintOutcome { status: "failed".into(), message: Some(e), job_id: Some(job_id.into()) }
                }
            })
        })();
        res.unwrap_or_else(|e| PrintOutcome { status: "failed".into(), message: Some(e.message), job_id: Some(job_id.into()) })
    }

    /// Run all pending jobs for a record (receipt first, then drawer pulse).
    pub fn print_pending_for(&self, ref_id: &str) {
        let jobs: Vec<String> = self
            .db
            .read(|c| {
                let mut st = c.prepare("SELECT job_id FROM print_jobs WHERE ref_id=?1 AND status IN ('pending','failed') ORDER BY CASE kind WHEN 'drawer' THEN 1 ELSE 0 END, created_at")?;
                let rows = st.query_map([ref_id], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .unwrap_or_default();
        for j in jobs {
            self.print_job_run(&j);
        }
    }

    pub fn print_retry(&self, token: &str, job_id: &str) -> AppResult<PrintOutcome> {
        let s = self.session(token)?;
        if !s.has("pos.sell") && !s.has("pos.reprint") {
            return Err(AppError::forbidden("pos.sell"));
        }
        let id = validate::id(job_id, "Print job")?;
        // Re-open cancelled jobs (e.g. printer configured afterwards).
        self.db.write(|tx| {
            tx.execute("UPDATE print_jobs SET status='pending' WHERE job_id=?1 AND status IN ('failed','cancelled')", [&id])?;
            Ok(())
        })?;
        Ok(self.print_job_run(&id))
    }

    pub fn print_queue(&self, token: &str) -> AppResult<Vec<PrintJobRow>> {
        let s = self.session(token)?;
        if !s.has("pos.sell") && !s.has("settings.manage") {
            return Err(AppError::forbidden("pos.sell"));
        }
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT j.job_id, j.kind, j.ref_id,
                    CASE j.kind WHEN 'sale' THEN (SELECT receipt_number FROM sales WHERE sale_id=j.ref_id)
                                WHEN 'refund' THEN (SELECT refund_receipt_number FROM refunds WHERE refund_id=j.ref_id)
                                WHEN 'shift_report' THEN (SELECT shift_number FROM shifts WHERE shift_id=j.ref_id) END,
                    j.status, j.attempts, j.last_error, j.created_at
                 FROM print_jobs j WHERE j.status IN ('pending','failed') AND j.kind <> 'drawer' ORDER BY j.created_at DESC LIMIT 100",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok(PrintJobRow {
                        job_id: r.get(0)?,
                        kind: r.get(1)?,
                        ref_id: r.get(2)?,
                        reference: r.get(3)?,
                        status: r.get(4)?,
                        attempts: r.get(5)?,
                        last_error: r.get(6)?,
                        created_at: r.get(7)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// Text preview of a receipt (sale/refund/shift_report), for the UI.
    pub fn receipt_preview(&self, token: &str, kind: &str, ref_id: &str) -> AppResult<serde_json::Value> {
        let s = self.session(token)?;
        if !s.has("pos.sell") && !s.has("sales.view") {
            return Err(AppError::forbidden("sales.view"));
        }
        let id = validate::id(ref_id, "Record")?;
        if !["sale", "refund", "shift_report"].contains(&kind) {
            return Err(AppError::validation("Unknown receipt type."));
        }
        self.db.read(|c| {
            let doc = render_job(c, kind, &id, None)?.ok_or_else(|| AppError::validation("Unknown receipt type."))?;
            Ok(serde_json::json!({ "text": doc.to_text(), "width_chars": doc.width_chars, "blocks": doc.blocks }))
        })
    }

    pub fn printer_test(&self, token: &str) -> AppResult<PrintOutcome> {
        let s = self.session(token)?;
        s.require("settings.manage")?;
        let job = self.db.write(|tx| enqueue(tx, "test", "test", None, Some(&s.user_id)))?;
        Ok(self.print_job_run(&job))
    }

    pub fn printer_drawer_test(&self, token: &str) -> AppResult<PrintOutcome> {
        let s = self.session(token)?;
        s.require("settings.manage")?;
        let actor = self.actor(&s, None);
        let job = self.db.write(|tx| {
            audit::record(tx, &actor, "drawer.test_opened", "device", Some(&s.device_id), None, None)?;
            enqueue(tx, "drawer", "test", None, Some(&s.user_id))
        })?;
        Ok(self.print_job_run(&job))
    }

    pub fn printers_available(&self, token: &str) -> AppResult<Vec<String>> {
        let s = self.session(token)?;
        s.require("settings.manage")?;
        Ok(system_printers())
    }
}

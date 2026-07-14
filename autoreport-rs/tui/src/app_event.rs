//! Bus event reduction for the terminal application.

use crate::app::Tui;
use crate::app_state::{Cell, PendingApproval, SysKind, ToolEntry};
use crate::config_update::Outcome;
use autoreport_core::config::save_settings;
use autoreport_core::types::BusMessage;
use serde_json::Value;

impl Tui {
    pub(crate) fn apply_bus(&mut self, msg: BusMessage) {
        match msg {
            BusMessage::AgentResponse {
                agent_type,
                content,
                streaming,
            } => {
                if !content.is_empty() && streaming {
                    if let Some(Cell::Assistant {
                        agent,
                        text,
                        streaming: true,
                    }) = self.history.last_mut()
                    {
                        if *agent == agent_type {
                            text.push_str(&content);
                            return;
                        }
                    }
                    self.history.push(Cell::Assistant {
                        agent: agent_type,
                        text: content,
                        streaming: true,
                    });
                } else if !streaming {
                    for cell in self.history.iter_mut().rev() {
                        if let Cell::Assistant {
                            agent, streaming, ..
                        } = cell
                        {
                            if *agent == agent_type {
                                *streaming = false;
                                break;
                            }
                        }
                    }
                }
            }
            BusMessage::AgentReasoning {
                agent_type,
                content,
                streaming,
            } => {
                if !content.is_empty() && streaming {
                    if let Some(Cell::Reasoning {
                        agent,
                        text,
                        streaming: true,
                    }) = self.history.last_mut()
                    {
                        if *agent == agent_type {
                            text.push_str(&content);
                            return;
                        }
                    }
                    self.history.push(Cell::Reasoning {
                        agent: agent_type,
                        text: content,
                        streaming: true,
                    });
                } else if !streaming {
                    for cell in self.history.iter_mut().rev() {
                        if let Cell::Reasoning {
                            agent, streaming, ..
                        } = cell
                        {
                            if *agent == agent_type {
                                *streaming = false;
                                break;
                            }
                        }
                    }
                }
            }
            BusMessage::ToolCall {
                agent_type,
                tool_name,
                arguments,
            } => {
                if let Some(Cell::ToolGroup { agent, items }) = self.history.last_mut() {
                    if *agent == agent_type {
                        items.push(ToolEntry {
                            name: tool_name,
                            args: arguments,
                            result: None,
                            error: None,
                        });
                        return;
                    }
                }
                self.history.push(Cell::ToolGroup {
                    agent: agent_type,
                    items: vec![ToolEntry {
                        name: tool_name,
                        args: arguments,
                        result: None,
                        error: None,
                    }],
                });
            }
            BusMessage::ToolResult {
                agent_type,
                tool_name,
                result,
                error,
            } => {
                let mut matched = false;
                for cell in self.history.iter_mut().rev() {
                    if let Cell::ToolGroup { agent, items } = cell {
                        if *agent == agent_type {
                            if let Some(item) = items
                                .iter_mut()
                                .rev()
                                .find(|item| item.name == tool_name && item.result.is_none())
                            {
                                item.result = Some(result.clone());
                                item.error = error.clone();
                                matched = true;
                                break;
                            }
                        }
                    }
                }
                if !matched {
                    self.history.push(Cell::ToolGroup {
                        agent: agent_type,
                        items: vec![ToolEntry {
                            name: tool_name,
                            args: Value::Null,
                            result: Some(result),
                            error,
                        }],
                    });
                }
            }
            BusMessage::StatusChange { agent_type, status } => {
                self.statuses.insert(agent_type, status);
            }
            BusMessage::SystemNotice {
                agent_type,
                content,
            } => {
                let label = agent_type
                    .map(|a| a.label().to_string())
                    .unwrap_or_else(|| "system".into());
                self.system(&format!("{label}: {content}"), SysKind::Info);
            }
            BusMessage::Error { message, .. } => {
                self.system(&format!("error: {message}"), SysKind::Error);
            }
            BusMessage::ApprovalRequest {
                agent_type,
                call_id,
                command,
                cwd,
                summary,
                reason,
            } => {
                // Single shared approval queue: any agent's request lands here
                // regardless of which agent is focused. Ported from codex's
                // `ApprovalOverlay::enqueue_request`.
                self.pending_approvals.push_back(PendingApproval {
                    agent: agent_type,
                    call_id,
                    command,
                    cwd,
                    summary,
                    reason,
                });
            }
            // Report messages resolve Main's send_to_agent internally; not a
            // user-visible cell.
            _ => {}
        }
    }
}

use crossterm::event::{Event, KeyCode, KeyModifiers};

impl Tui {
    pub(crate) fn handle_event(&mut self, ev: Event) -> bool {
        let Event::Key(key) = ev else {
            return true;
        };

        // While an approval popup is open, it owns all keys (codex semantics:
        // the modal must emit an explicit decision before anything else runs).
        if !self.pending_approvals.is_empty() {
            self.handle_approval_key(key);
            return true;
        }

        // While a configuration overlay is open, route all keys to it.
        if let Some(screen) = self.overlay.as_mut() {
            if let Some(outcome) = screen.handle_key(key) {
                match outcome {
                    Outcome::Saved => {
                        if let Err(e) = save_settings(&self.workspace, screen.settings()) {
                            self.system(&format!("config save failed: {e}"), SysKind::Error);
                        } else {
                            self.system(
                                "configuration saved to autoreport.config.yaml — restart to apply",
                                SysKind::Info,
                            );
                        }
                    }
                    Outcome::Cancelled => {
                        self.system("config unchanged", SysKind::Info);
                    }
                }
                self.overlay = None;
            }
            return true;
        }

        // While a completion popup is open, intercept navigation keys.
        if self.slash.is_some() {
            match key.code {
                KeyCode::Down => {
                    self.move_slash(1);
                    return true;
                }
                KeyCode::Up => {
                    self.move_slash(-1);
                    return true;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    self.accept_slash();
                    return true;
                }
                KeyCode::Esc => {
                    self.slash = None;
                    return true;
                }
                _ => {}
            }
        }
        if self.mention.is_some() {
            match key.code {
                KeyCode::Down => {
                    self.move_mention(1);
                    return true;
                }
                KeyCode::Up => {
                    self.move_mention(-1);
                    return true;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    self.accept_mention();
                    return true;
                }
                KeyCode::Esc => {
                    self.mention = None;
                    return true;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Enter => self.submit(),
            KeyCode::Tab => self.cycle_agent(),
            KeyCode::BackTab => self.cycle_agent_back(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    // step back one char
                    let prev = self.input[..self.cursor].chars().last().unwrap();
                    self.cursor -= prev.len_utf8();
                    self.input.remove(self.cursor);
                }
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    let prev = self.input[..self.cursor].chars().last().unwrap();
                    self.cursor -= prev.len_utf8();
                }
            }
            KeyCode::Right => {
                if self.cursor < self.input.len() {
                    let next = self.input[self.cursor..].chars().next().unwrap();
                    self.cursor += next.len_utf8();
                }
            }
            KeyCode::Up => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Down => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Esc => {
                // ESC: close completion popups if open, otherwise interrupt
                // the focused agent's active turn (codex semantics).
                if self.slash.is_some() {
                    self.slash = None;
                } else if self.mention.is_some() {
                    self.mention = None;
                } else {
                    self.manager.interrupt(self.focused);
                }
            }
            KeyCode::Char(c) => {
                self.input.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            _ => {}
        }

        // Any input that wasn't intercepted resets manual scroll.
        if !matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        ) {
            self.scroll = 0;
        }

        // Recompute completion popups based on the token under the cursor.
        self.recompute_slash();
        self.recompute_mention();
        true
    }
}

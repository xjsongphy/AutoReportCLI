//! Shared task board used by `update_plan` / `send_to_agent` to coordinate
//! Main ↔ sub-agent work. Thread-safe via a mutex.

use crate::types::{AgentType, TaskItem, TaskStatus};
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct TaskBoard {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    tasks: HashMap<String, TaskItem>,
    counter: u64,
}

impl TaskBoard {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                tasks: HashMap::new(),
                counter: 0,
            })),
        }
    }

    fn next_id(&self, g: &mut Inner) -> String {
        g.counter += 1;
        format!("T{}", g.counter)
    }

    pub fn create(
        &self,
        source: AgentType,
        target: AgentType,
        brief: String,
        blocking: bool,
        session_id: Option<String>,
    ) -> TaskItem {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let id = self.next_id(&mut g);
        let task = TaskItem {
            task_id: id.clone(),
            brief,
            source_agent: source,
            target_agent: target,
            status: TaskStatus::Pending,
            created_at: Utc::now(),
            completed_at: None,
            blocking,
            session_id,
            plan_order: None,
            reply: None,
        };
        g.tasks.insert(id, task.clone());
        task
    }

    pub fn add_local(&self, agent: AgentType, brief: String) -> TaskItem {
        self.create(agent, agent, brief, false, None)
    }

    fn set_status(&self, task_id: &str, status: TaskStatus) -> Option<TaskItem> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let task = g.tasks.get_mut(task_id)?;
        task.status = status;
        task.completed_at = status.is_settled().then(Utc::now);
        Some(task.clone())
    }

    pub fn start(&self, task_id: &str) -> Option<TaskItem> {
        self.set_status(task_id, TaskStatus::InProgress)
    }
    pub fn complete(&self, task_id: &str, reply: Option<String>) -> Option<TaskItem> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let task = g.tasks.get_mut(task_id)?;
        task.status = TaskStatus::Completed;
        task.completed_at = Some(Utc::now());
        if let Some(r) = reply {
            task.reply = Some(r);
        }
        Some(task.clone())
    }
    pub fn fail(&self, task_id: &str) -> Option<TaskItem> {
        self.set_status(task_id, TaskStatus::Failed)
    }
    pub fn cancel(&self, task_id: &str) -> Option<TaskItem> {
        self.set_status(task_id, TaskStatus::Cancelled)
    }
    /// Mark a delegated task BLOCKED — the target cannot proceed and needs the
    /// dispatcher (source) to act. Returns the updated task.
    pub fn block(&self, task_id: &str) -> Option<TaskItem> {
        self.set_status(task_id, TaskStatus::Blocked)
    }

    /// Look up a task by id with optional filters. `active_only` restricts to
    /// pending/in_progress.
    pub fn get_task(
        &self,
        task_id: &str,
        target_agent: Option<AgentType>,
        active_only: bool,
    ) -> Option<TaskItem> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.tasks
            .get(task_id)
            .filter(|t| {
                target_agent.is_none_or(|a| t.target_agent == a)
                    && (!active_only
                        || matches!(t.status, TaskStatus::Pending | TaskStatus::InProgress))
            })
            .cloned()
    }

    fn is_local_plan_task(task: &TaskItem, agent: AgentType) -> bool {
        task.source_agent == agent && task.target_agent == agent
    }

    fn sort_tasks(tasks: &mut [TaskItem]) {
        tasks.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.task_id.cmp(&b.task_id))
        });
    }

    fn sort_plan(tasks: &mut [TaskItem]) {
        tasks.sort_by(|a, b| {
            a.plan_order
                .unwrap_or(u32::MAX)
                .cmp(&b.plan_order.unwrap_or(u32::MAX))
                .then(a.created_at.cmp(&b.created_at))
                .then(a.task_id.cmp(&b.task_id))
        });
    }

    /// Codex-style local plan for `agent`, backed by self-assigned tasks.
    pub fn local_plan(&self, agent: AgentType) -> Vec<TaskItem> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut tasks: Vec<TaskItem> = g
            .tasks
            .values()
            .filter(|t| Self::is_local_plan_task(t, agent))
            .cloned()
            .collect();
        Self::sort_plan(&mut tasks);
        tasks
    }

    /// Replace the local plan for `agent` with the provided ordered steps.
    pub fn sync_local_plan(
        &self,
        agent: AgentType,
        steps: Vec<(String, TaskStatus)>,
    ) -> Vec<TaskItem> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let existing_local: Vec<TaskItem> = g
            .tasks
            .values()
            .filter(|t| Self::is_local_plan_task(t, agent))
            .cloned()
            .collect();
        let mut reusable: HashMap<String, Vec<TaskItem>> = HashMap::new();
        for task in &existing_local {
            reusable
                .entry(task.brief.clone())
                .or_default()
                .push(task.clone());
        }

        let mut kept_ids = HashSet::new();
        for (order, (brief, status)) in steps.into_iter().enumerate() {
            let mut task = reusable
                .get_mut(&brief)
                .and_then(|tasks| tasks.pop())
                .unwrap_or_else(|| TaskItem {
                    task_id: self.next_id(&mut g),
                    brief: brief.clone(),
                    source_agent: agent,
                    target_agent: agent,
                    status,
                    created_at: Utc::now(),
                    completed_at: None,
                    blocking: false,
                    session_id: None,
                    plan_order: None,
                    reply: None,
                });
            task.brief = brief;
            task.status = status;
            task.completed_at = status.is_settled().then(Utc::now);
            task.blocking = false;
            task.session_id = None;
            task.plan_order = Some(order as u32);
            task.reply = None;
            kept_ids.insert(task.task_id.clone());
            g.tasks.insert(task.task_id.clone(), task);
        }

        for task in existing_local {
            if !kept_ids.contains(&task.task_id) {
                g.tasks.remove(&task.task_id);
            }
        }

        let mut tasks: Vec<TaskItem> = g
            .tasks
            .values()
            .filter(|t| Self::is_local_plan_task(t, agent))
            .cloned()
            .collect();
        Self::sort_plan(&mut tasks);
        tasks
    }

    /// Tasks assigned *to* `agent` that are still open (pending / in progress).
    pub fn todolist(&self, agent: AgentType) -> Vec<TaskItem> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut tasks: Vec<TaskItem> = g
            .tasks
            .values()
            .filter(|t| {
                t.target_agent == agent
                    && matches!(t.status, TaskStatus::Pending | TaskStatus::InProgress)
            })
            .cloned()
            .collect();
        Self::sort_tasks(&mut tasks);
        tasks
    }

    /// Tasks `agent` assigned *to others* that are still open.
    pub fn waitlist(&self, agent: AgentType) -> Vec<TaskItem> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut tasks: Vec<TaskItem> = g
            .tasks
            .values()
            .filter(|t| {
                t.source_agent == agent
                    && t.target_agent != agent
                    && matches!(t.status, TaskStatus::Pending | TaskStatus::InProgress)
            })
            .cloned()
            .collect();
        Self::sort_tasks(&mut tasks);
        tasks
    }

    /// Tasks `agent` dispatched that are currently BLOCKED (need its action).
    pub fn blocked_waitlist(&self, agent: AgentType) -> Vec<TaskItem> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut tasks: Vec<TaskItem> = g
            .tasks
            .values()
            .filter(|t| {
                t.source_agent == agent
                    && t.target_agent != agent
                    && t.status == TaskStatus::Blocked
            })
            .cloned()
            .collect();
        Self::sort_tasks(&mut tasks);
        tasks
    }

    pub fn all(&self) -> Vec<TaskItem> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tasks
            .values()
            .cloned()
            .collect()
    }
}

impl Default for TaskBoard {
    fn default() -> Self {
        Self::new()
    }
}

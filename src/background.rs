/// Background task tree tracking.
///
/// background_tasks_changed is a level signal (replace semantics).
/// Feed it `AgentEvent::BackgroundTasks`; background a running task with
/// `session.control(&BackgroundTasksRequest::one(tool_use_id))`.
/// Source: binary background_tasks_changed + background_tasks schemas
use crate::protocol::BackgroundTask;

/// Tracks the current agent task tree from background_tasks_changed events.
#[derive(Default)]
pub struct TaskTree {
    tasks: Vec<BackgroundTask>,
}

impl TaskTree {
    pub fn new() -> Self { Self::default() }

    /// Update with a new snapshot (replace semantics).
    pub fn update(&mut self, tasks: Vec<BackgroundTask>) {
        self.tasks = tasks;
    }

    pub fn tasks(&self) -> &[BackgroundTask] { &self.tasks }

    /// Find a task by ID.
    pub fn get(&self, id: &str) -> Option<&BackgroundTask> {
        self.tasks.iter().find(|t| t.task_id == id)
    }

    /// All direct children of a parent task.
    pub fn children(&self, parent_id: &str) -> Vec<&BackgroundTask> {
        self.tasks.iter()
            .filter(|t| t.parent_task_id.as_deref() == Some(parent_id))
            .collect()
    }
}

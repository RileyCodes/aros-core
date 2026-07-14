//! Memory system — global memory + guide mode (task-specific context)

use std::sync::{Arc, RwLock};

/// Global persistent memory (personal info, preferences, long-term notes)
#[derive(Clone)]
pub struct MemoryManager {
    content: Arc<RwLock<String>>,
}

impl MemoryManager {
    pub fn new() -> Self {
        Self {
            content: Arc::new(RwLock::new(String::new())),
        }
    }

    pub fn get(&self) -> String {
        self.content.read().unwrap().clone()
    }

    pub fn set(&self, value: &str) {
        *self.content.write().unwrap() = value.to_string();
    }

    pub fn clear(&self) {
        self.content.write().unwrap().clear();
    }

    pub fn is_empty(&self) -> bool {
        self.content.read().unwrap().is_empty()
    }
}

impl Default for MemoryManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Guide Mode — temporary task-specific context that modifies prompts
/// Examples: phone call script, meeting agenda, shopping list
#[derive(Clone)]
pub struct GuideMode {
    inner: Arc<RwLock<Option<GuideData>>>,
}

#[derive(Clone, Debug)]
pub struct GuideData {
    /// Short label shown in status bar (e.g., "📞 電話", "🏥 診察")
    pub label: String,
    /// Goal of this guide (e.g., "予約する")
    pub goal: String,
    /// Context/script for suggestions (e.g., expected Q&A pairs)
    pub context: String,
}

impl GuideMode {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
        }
    }

    /// Activate guide mode with goal and context
    pub fn activate(&self, label: &str, goal: &str, context: &str) {
        *self.inner.write().unwrap() = Some(GuideData {
            label: label.to_string(),
            goal: goal.to_string(),
            context: context.to_string(),
        });
        log::info!("Guide mode ON: {} — {}", label, goal);
    }

    /// Deactivate guide mode
    pub fn deactivate(&self) {
        *self.inner.write().unwrap() = None;
        log::info!("Guide mode OFF");
    }

    /// Get current guide data (None if inactive)
    pub fn get(&self) -> Option<GuideData> {
        self.inner.read().unwrap().clone()
    }

    /// Is guide mode active?
    pub fn is_active(&self) -> bool {
        self.inner.read().unwrap().is_some()
    }

    /// Get status bar label (empty if inactive)
    pub fn label(&self) -> String {
        self.inner
            .read()
            .unwrap()
            .as_ref()
            .map(|g| g.label.clone())
            .unwrap_or_default()
    }
}

impl Default for GuideMode {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_crud() {
        let mem = MemoryManager::new();
        assert!(mem.is_empty());
        mem.set("test");
        assert_eq!(mem.get(), "test");
        mem.clear();
        assert!(mem.is_empty());
    }

    #[test]
    fn test_memory_thread_safe() {
        let mem = MemoryManager::new();
        let mem2 = mem.clone();
        mem.set("test");
        assert_eq!(mem2.get(), "test");
    }

    #[test]
    fn test_guide_mode() {
        let guide = GuideMode::new();
        assert!(!guide.is_active());
        assert!(guide.get().is_none());

        guide.activate("📞", "予約する", "マンジャロ増量の相談");
        assert!(guide.is_active());
        assert_eq!(guide.label(), "📞");
        let data = guide.get().unwrap();
        assert_eq!(data.goal, "予約する");

        guide.deactivate();
        assert!(!guide.is_active());
    }
}

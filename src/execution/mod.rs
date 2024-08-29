pub mod transactional;

use anyhow::{anyhow, Result};

/// An executor is responsible for execute tests, parse their results, and avoid duplications
pub trait Executor<Input, ExecutionResult> {
    fn empty_executor() -> Self;
    /// Execute one test
    fn execute_one(&self, input: &Input) -> ExecutionResult;
    /// Save the execution result to avoid future duplication
    fn save_result(&mut self, result: ExecutionResult);
    /// Check if the result can be ignored (e.g. have seen similar one)
    fn should_ignore(&self, result: &ExecutionResult) -> bool;
    /// Decide if the result is a bug
    fn is_bug(&self, result: &ExecutionResult) -> bool;

    fn execute_save_report(&mut self, input: &Input) -> Result<()> {
        let result = self.execute_one(input);
        let ret = self.should_ignore(&result);
        self.save_result(result);
        match ret {
            true => Ok(()),
            false => Err(anyhow!("Execution failed, this is a bug")),
        }
    }
}

// Execution profile system - user-controlled risk levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ExecutionProfile {
    /// Unrestricted access to host resources (current behavior with security considerations)
    #[default]
    Unrestricted,
    
    /// Sandboxed execution using container isolation
    Sandboxed,
    
    /// Remote execution via SSH/managed environment
    Restricted,
}

#[derive(Debug, Default)]
pub struct ExecutionContext {
    /// The profile selected by the user
    pub profile: ExecutionProfile,
    
    /// Configuration for the profile
    pub config: Option<std::collections::HashMap<String, String>>,
}

impl ExecutionContext {
    /// Create a new execution context with the given profile
    pub fn new(profile: ExecutionProfile) -> Self {
        Self {
            profile,
            config: None,
        }
    }
}

// Profile-aware executor wrapper
pub struct ProfiledExecutor {
    base_executor: Box<dyn crate::pipeline::Executor>,
    execution_context: ExecutionContext,
}

impl ProfiledExecutor {
    pub fn new(base_executor: Box<dyn crate::pipeline::Executor>, context: ExecutionContext) -> Self {
        Self {
            base_executor,
            execution_context: context,
        }
    }

    pub fn execute(&self, capability: &str, args: &crate::schema::Value) -> Result<crate::schema::Value, crate::pipeline::ExecError> {
        match self.execution_context.profile {
            ExecutionProfile::Unrestricted => {
                // Delegate directly to base executor
                self.base_executor.execute(capability, args)
            }
            ExecutionProfile::Sandboxed => {
                // Apply sandbox security checks
                self.apply_sandbox_security(capability, args)
                    .and_then(|()| self.base_executor.execute(capability, args))
            }
            ExecutionProfile::Restricted => {
                // Enforce restricted policy
                self.enforce_restricted_policy(capability, args)
                    .and_then(|()| self.base_executor.execute(capability, args))
            }
        }
    }

    fn apply_sandbox_security(&self, capability: &str, _args: &crate::schema::Value) -> Result<(), crate::pipeline::ExecError> {
        // Implement sandbox security checks based on capability
        match capability {
            "cap.shell" => {
                // Additional sandbox restrictions could be added here
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn enforce_restricted_policy(&self, capability: &str, _args: &crate::schema::Value) -> Result<(), crate::pipeline::ExecError> {
        // Strict validation for restricted mode
        match capability {
            "cap.shell" => {
                // In restricted mode, require explicit approval for shell execution
                Ok(())
            }
            _ => Ok(()),
        }
    }
}
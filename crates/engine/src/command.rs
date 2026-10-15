//! Command registry (AGENTS.md §2 Phase 3 item 2: "everything is a command").
//!
//! Every user-visible action is a registered [`Command`] with an id, a JSON-schema for its
//! params, an `enabled` predicate and `run`. UI, shortcuts, CLI and the control channel all
//! dispatch through [`Registry::execute`]. Commands never open dialogs.

use crate::{EngineError, Session};
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;

/// Static description of a command (also feeds the parity catalog).
#[derive(Clone, Debug, Serialize)]
pub struct Spec {
    pub id: &'static str,
    pub doc: &'static str,
    /// SPEC requirement ids this command implements (e.g. "R-ORG-002").
    pub reqs: &'static [&'static str],
    /// `false` for registered stubs whose `run` returns [`EngineError::NotImplemented`].
    pub implemented: bool,
}

pub trait Command: Send + Sync {
    fn spec(&self) -> &Spec;
    fn params_schema(&self) -> Value;
    fn enabled(&self, _s: &Session) -> bool {
        true
    }
    fn run(&self, s: &mut Session, params: Value) -> Result<Value, EngineError>;
}

/// Adapter that turns a typed function into a [`Command`].
pub struct Typed<P, R, F>
where
    F: Fn(&mut Session, P) -> Result<R, EngineError> + Send + Sync,
{
    pub spec: Spec,
    pub f: F,
    pub enabled: fn(&Session) -> bool,
    _p: std::marker::PhantomData<fn(P) -> R>,
}

impl<P, R, F> Typed<P, R, F>
where
    F: Fn(&mut Session, P) -> Result<R, EngineError> + Send + Sync,
{
    pub fn new(spec: Spec, f: F) -> Self {
        Typed {
            spec,
            f,
            enabled: |_| true,
            _p: std::marker::PhantomData,
        }
    }
    pub fn when(mut self, enabled: fn(&Session) -> bool) -> Self {
        self.enabled = enabled;
        self
    }
}

impl<P, R, F> Command for Typed<P, R, F>
where
    P: DeserializeOwned + JsonSchema,
    R: Serialize,
    F: Fn(&mut Session, P) -> Result<R, EngineError> + Send + Sync,
{
    fn spec(&self) -> &Spec {
        &self.spec
    }
    fn params_schema(&self) -> Value {
        serde_json::to_value(schemars::schema_for!(P)).unwrap_or(Value::Null)
    }
    fn enabled(&self, s: &Session) -> bool {
        (self.enabled)(s)
    }
    fn run(&self, s: &mut Session, params: Value) -> Result<Value, EngineError> {
        // `null` is accepted for parameterless commands.
        let params = if params.is_null() {
            Value::Object(Default::default())
        } else {
            params
        };
        let p: P = serde_json::from_value(params).map_err(|e| EngineError::InvalidParams {
            cmd: self.spec.id.to_string(),
            msg: e.to_string(),
        })?;
        let r = (self.f)(s, p)?;
        serde_json::to_value(r).map_err(|e| EngineError::Internal(e.to_string()))
    }
}

/// A registered command that is not implemented yet (tests for it start red).
pub struct Stub(pub Spec);

impl Command for Stub {
    fn spec(&self) -> &Spec {
        &self.0
    }
    fn params_schema(&self) -> Value {
        Value::Null
    }
    fn run(&self, _s: &mut Session, _params: Value) -> Result<Value, EngineError> {
        Err(EngineError::NotImplemented(self.0.id.to_string()))
    }
}

#[derive(Default)]
pub struct Registry {
    cmds: BTreeMap<&'static str, Box<dyn Command>>,
}

impl Registry {
    pub fn register(&mut self, c: Box<dyn Command>) -> Result<(), EngineError> {
        let id = c.spec().id;
        if self.cmds.contains_key(id) {
            return Err(EngineError::Internal(format!("duplicate command id {id}")));
        }
        self.cmds.insert(id, c);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&dyn Command> {
        self.cmds.get(id).map(|b| b.as_ref())
    }

    pub fn specs(&self) -> impl Iterator<Item = &Spec> {
        self.cmds.values().map(|c| c.spec())
    }

    pub fn ids(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.cmds.keys().copied()
    }

    /// Describes all commands (id, doc, reqs, implemented, params schema).
    pub fn describe(&self) -> Value {
        Value::Array(
            self.cmds
                .values()
                .map(|c| {
                    let mut v = serde_json::to_value(c.spec()).unwrap_or(Value::Null);
                    if let Value::Object(m) = &mut v {
                        m.insert("params".into(), c.params_schema());
                    }
                    v
                })
                .collect(),
        )
    }

    pub fn execute(&self, s: &mut Session, id: &str, params: Value) -> Result<Value, EngineError> {
        let c = self
            .get(id)
            .ok_or_else(|| EngineError::UnknownCommand(id.to_string()))?;
        if !c.enabled(s) {
            return Err(EngineError::Disabled(id.to_string()));
        }
        c.run(s, params)
    }
}

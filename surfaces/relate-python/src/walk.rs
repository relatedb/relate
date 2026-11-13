//! The one Python-container walk policy shared by [`crate::convert`] (Python
//! parameters in) and [`crate::graph_values`] (property values out): the
//! nesting depth cap and the container cycle guard. Both directions descend
//! the same list/dict vocabulary, so the policy lives below both.

use std::collections::HashSet;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

/// Maximum list/dict nesting depth accepted when converting parameters and when
/// copying properties for `data()` / `to_dict()`. Deeper structures raise
/// rather than recursing until the Rust stack overflows and aborts the host
/// interpreter (LABS-114).
pub(super) const PY_VALUE_DEPTH_CAP: usize = 128;

/// One direction's walk over the container tree: which containers are on the
/// current recursion path, and the context (`"parameter"` / `"property"`) the
/// depth error names. The path alone carries the depth — a container enters it
/// exactly once — and [`Walk::enter`] is the only place it changes, so no
/// caller can bypass the cap.
pub(super) struct Walk {
    context: &'static str,
    visiting: HashSet<usize>,
}

impl Walk {
    /// A walk over Python query parameters.
    pub(super) fn parameter() -> Self {
        Walk {
            context: "parameter",
            visiting: HashSet::new(),
        }
    }

    /// A walk over entity property values copied for `data()` / `to_dict()`.
    pub(super) fn property() -> Self {
        Walk {
            context: "property",
            visiting: HashSet::new(),
        }
    }

    /// Run `body` over the children of the container `obj`: check the cap,
    /// insert `obj` on the recursion path, run `body`, then remove `obj` again
    /// on the returning path — error or not. The one container bracket both
    /// directions route through.
    pub(super) fn enter<'py, T>(
        &mut self,
        obj: &Bound<'py, PyAny>,
        body: impl FnOnce(&mut Walk) -> PyResult<T>,
    ) -> PyResult<T> {
        if self.visiting.len() > PY_VALUE_DEPTH_CAP {
            return Err(PyValueError::new_err(format!(
                "{} nesting exceeds the maximum depth of {PY_VALUE_DEPTH_CAP}",
                self.context
            )));
        }
        // `visiting` holds the `PyObject*` of every container on the current
        // recursion path, so a list/dict that (transitively) contains itself is
        // rejected instead of recursing forever. A repeated container is never
        // inserted, so the matching remove below is reached only on success.
        if !self.visiting.insert(obj.as_ptr() as usize) {
            return Err(PyValueError::new_err(
                "value contains a cycle: a list or map references itself",
            ));
        }
        let result = body(self);
        self.visiting.remove(&(obj.as_ptr() as usize));
        result
    }
}

pub mod ndarray;
use std::fmt::Write;

use ndarray::NdArray;
use pyo3::{exceptions::PyValueError, prelude::*, wrap_pyfunction};

// TODO: once this is stable use a macro to generate for a variaty of types...
//
#[pyclass]
#[derive(Debug)]
pub struct NdArrayD {
    inner: NdArray<f64>,
}

#[pymethods]
impl NdArrayD {
    #[new]
    pub fn new(dims: Vec<u32>) -> Self {
        Self {
            inner: NdArray::new(dims.into_boxed_slice()),
        }
    }

    pub fn get(&self, index: Vec<u32>) -> Option<f64> {
        self.inner.get(&index).cloned()
    }

    pub fn set(&mut self, index: Vec<u32>, value: f64) {
        if let Some(x) = self.inner.get_mut(&index) {
            *x = value;
        }
    }

    /// The values must have a length equal to the product of the dimensions!
    pub fn set_values(&mut self, values: Vec<f64>) -> PyResult<()> {
        self.inner
            .set_slice(values.into_boxed_slice())
            .map(|_| ())
            .map_err(|err| PyValueError::new_err::<String>(format!("{}", err).into()))
    }

    // TODO __str__
    pub fn to_string(&self) -> String {
        let depth = self.inner.dims().len();
        let mut s = String::with_capacity(self.inner.len() * 4);
        for _ in 0..depth - 1 {
            s.push('[');
        }
        let mut it = self.inner.iter_cols();
        if let Some(col) = it.next() {
            write!(s, "{:?}", col).unwrap();
        }
        for col in it {
            write!(s, "\n{:?}", col).unwrap();
        }
        for _ in 0..depth - 1 {
            s.push(']');
        }
        s
    }
}

/// Nd array of 64 bit floats
#[pyfunction]
pub fn make_ndf64(dims: Vec<u32>, values: Vec<f64>) -> PyResult<NdArrayD> {
    let mut arr = NdArrayD::new(dims);
    if let Err(err) = arr.inner.set_slice(values.into_boxed_slice()) {
        return Err(PyValueError::new_err::<String>(format!("{}", err).into()));
    }
    Ok(arr)
}

#[pymodule]
fn nd(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<NdArrayD>()?;
    m.add_function(wrap_pyfunction!(make_ndf64, m)?)?;

    Ok(())
}

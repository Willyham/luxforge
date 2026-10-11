//! A minimal command-line reader: `--name value` options, `--flag` switches and positionals.

use std::path::PathBuf;

pub struct Args {
    rest: Vec<String>,
}

impl Args {
    pub fn new(rest: Vec<String>) -> Self {
        Self { rest }
    }

    /// Removes and returns every value of a repeatable option.
    pub fn all(&mut self, name: &str) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        while let Some(v) = self.opt(name)? {
            out.push(v);
        }
        Ok(out)
    }

    pub fn opt(&mut self, name: &str) -> Result<Option<String>, String> {
        let Some(i) = self.rest.iter().position(|a| a == name) else {
            return Ok(None);
        };
        if i + 1 >= self.rest.len() {
            return Err(format!("{name} needs a value"));
        }
        let value = self.rest.remove(i + 1);
        self.rest.remove(i);
        Ok(Some(value))
    }

    pub fn required(&mut self, name: &str) -> Result<String, String> {
        self.opt(name)?.ok_or_else(|| format!("{name} is required"))
    }

    pub fn path(&mut self, name: &str) -> Result<Option<PathBuf>, String> {
        Ok(self.opt(name)?.map(PathBuf::from))
    }

    pub fn parsed<T: std::str::FromStr>(&mut self, name: &str) -> Result<Option<T>, String> {
        match self.opt(name)? {
            None => Ok(None),
            Some(v) => v
                .parse()
                .map(Some)
                .map_err(|_| format!("{name}: cannot parse {v:?}")),
        }
    }

    pub fn flag(&mut self, name: &str) -> bool {
        match self.rest.iter().position(|a| a == name) {
            Some(i) => {
                self.rest.remove(i);
                true
            }
            None => false,
        }
    }

    /// The first remaining argument that is not an option. Read every option first, or an option's
    /// value can be taken for a positional.
    pub fn positional(&mut self) -> Option<String> {
        let i = self.rest.iter().position(|a| !a.starts_with("--"))?;
        Some(self.rest.remove(i))
    }

    pub fn done(self) -> Result<(), String> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(format!("unexpected arguments: {}", self.rest.join(" ")))
        }
    }
}

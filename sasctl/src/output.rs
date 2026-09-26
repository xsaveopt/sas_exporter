use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
}

pub trait Render {
    fn render(&self, out: &mut String);
}

pub trait Emit {
    fn json(&self) -> anyhow::Result<serde_json::Value>;
    fn text(&self) -> String;
}

impl std::fmt::Debug for dyn Emit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text())
    }
}

impl<T: Serialize + Render> Emit for T {
    fn json(&self) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::to_value(self)?)
    }

    fn text(&self) -> String {
        let mut out = String::new();
        self.render(&mut out);
        out
    }
}

#[derive(Default)]
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new<I, S>(headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            headers: headers.into_iter().map(Into::into).collect(),
            rows: Vec::new(),
        }
    }

    pub fn row<I, S>(&mut self, cells: I)
    where
        I: IntoIterator<Item = S>,
        S: ToString,
    {
        self.rows
            .push(cells.into_iter().map(|c| c.to_string()).collect());
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn render(&self, out: &mut String) {
        let cols = self.headers.len();
        let mut widths: Vec<usize> = self.headers.iter().map(|h| h.chars().count()).collect();
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate().take(cols) {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }
        let line = |cells: &[String], out: &mut String| {
            let mut s = String::new();
            for (i, w) in widths.iter().enumerate() {
                let cell = cells.get(i).map(String::as_str).unwrap_or("");
                if i + 1 == cols {
                    s.push_str(cell);
                } else {
                    s.push_str(&format!("{cell:<w$}  "));
                }
            }
            out.push_str(s.trim_end());
            out.push('\n');
        };
        line(&self.headers, out);
        let rule: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
        line(&rule, out);
        for row in &self.rows {
            line(row, out);
        }
    }
}

#[derive(Default)]
pub struct Fields {
    title: Option<String>,
    items: Vec<(String, String)>,
}

impl Fields {
    pub fn titled(title: impl Into<String>) -> Self {
        Self {
            title: Some(title.into()),
            items: Vec::new(),
        }
    }

    pub fn add(&mut self, key: impl Into<String>, value: impl ToString) -> &mut Self {
        self.items.push((key.into(), value.to_string()));
        self
    }

    pub fn render(&self, out: &mut String) {
        if let Some(t) = &self.title {
            out.push_str(t);
            out.push('\n');
            out.push_str(&"=".repeat(t.chars().count()));
            out.push('\n');
        }
        let width = self
            .items
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(0);
        for (k, v) in &self.items {
            out.push_str(&format!("{k:<width$} : {v}\n"));
        }
    }
}

#[derive(Serialize)]
pub struct Done {
    pub status: &'static str,
    pub message: String,
}

impl Done {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            status: "ok",
            message: message.into(),
        }
    }
}

impl Render for Done {
    fn render(&self, out: &mut String) {
        out.push_str(&self.message);
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_aligns_columns() {
        let mut t = Table::new(["ID", "Name"]);
        t.row(["0", "SAS2008"]);
        t.row(["10", "SAS3008"]);
        let mut out = String::new();
        t.render(&mut out);
        assert_eq!(out, "ID  Name\n--  -------\n0   SAS2008\n10  SAS3008\n");
    }

    #[test]
    fn fields_pad_keys() {
        let mut f = Fields::titled("Controller");
        f.add("Firmware", "20.00.07.00").add("BIOS", "7.39");
        let mut out = String::new();
        f.render(&mut out);
        assert_eq!(
            out,
            "Controller\n==========\nFirmware : 20.00.07.00\nBIOS     : 7.39\n"
        );
    }
}

mod bytes;
mod cli;
mod controllers;
mod ioctl;
mod mega;
mod mpi3;
mod mpt;
mod output;
mod sysfs;

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, anyhow, bail};
use clap::{ArgMatches, CommandFactory, FromArgMatches};

use crate::cli::{Cli, Command, Scope};
use crate::controllers::{Backend, Controller};
use crate::output::{Emit, Format};

pub struct Ctx {
    pub format: Format,
    pub yes: bool,
    pub interactive: bool,
    pub sysfs: PathBuf,
}

impl Ctx {
    pub fn confirm(&self, what: &str) -> Result<()> {
        if self.yes {
            return Ok(());
        }
        if !self.interactive {
            bail!("{what} changes the controller, rerun with --yes to confirm");
        }
        eprint!("About to go ahead with {what}. Continue? [y/N] ");
        std::io::stderr().flush().ok();
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        match answer.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => Ok(()),
            _ => bail!("cancelled, nothing was changed"),
        }
    }
}

fn command_path(m: &ArgMatches) -> String {
    let mut words = Vec::new();
    let mut cur = m;
    while let Some((name, sub)) = cur.subcommand() {
        words.push(name.to_string());
        cur = sub;
    }
    words.join(" ")
}

fn supports(c: &Controller, cmd: &Command) -> bool {
    match &c.backend {
        Backend::Mpt(_) => mpt::cli::supports(cmd),
        Backend::Mpi3(_) => mpi3::cli::supports(cmd),
        Backend::Mega(_) => mega::cli::supports(cmd),
    }
}

fn execute(c: &Controller, cmd: &Command, ctx: &Ctx) -> Result<Box<dyn Emit>> {
    match &c.backend {
        Backend::Mpt(target) => {
            let t = mpt::adapter::open(target)?;
            mpt::cli::execute(cmd, ctx, target, t.as_ref())
        }
        Backend::Mpi3(target) => {
            let t = mpi3::adapter::open(target)?;
            mpi3::cli::execute(cmd, ctx, target, t.as_ref())
        }
        Backend::Mega(r) => {
            let t = mega::open_host(r.host_no)?;
            mega::cli::execute(cmd, r, t.as_ref(), ctx)
        }
    }
}

fn pick<'a>(
    ctrls: &'a [Controller],
    wanted: Option<usize>,
    cmd: &Command,
    what: &str,
) -> Result<Vec<&'a Controller>> {
    if ctrls.is_empty() {
        bail!("no supported controllers found");
    }
    if let Some(id) = wanted {
        let c = ctrls.get(id).ok_or_else(|| {
            anyhow!(
                "controller {id} does not exist, sasctl controller lists the {} found",
                ctrls.len()
            )
        })?;
        if !supports(c, cmd) {
            bail!(
                "{what} is not available on controller {id}, which runs {}",
                c.driver
            );
        }
        return Ok(vec![c]);
    }
    let able: Vec<&Controller> = ctrls.iter().filter(|c| supports(c, cmd)).collect();
    if able.is_empty() {
        let drivers: Vec<&str> = ctrls.iter().map(|c| c.driver.as_str()).collect();
        bail!("{what} is not available on {}", drivers.join(", "));
    }
    if cmd.scope() == Scope::One && able.len() > 1 {
        let ids: Vec<String> = able
            .iter()
            .map(|c| format!("{} ({})", c.id, c.driver))
            .collect();
        bail!(
            "{what} needs one controller and {} can do it, pick one with -c: {}",
            able.len(),
            ids.join(", ")
        );
    }
    Ok(able)
}

fn envelope(c: &Controller, value: Result<serde_json::Value>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("controller".into(), c.id.into());
    map.insert("driver".into(), c.driver.clone().into());
    match value {
        Ok(serde_json::Value::Object(fields)) => {
            for (k, v) in fields {
                map.entry(k).or_insert(v);
            }
        }
        Ok(other) => {
            map.insert("data".into(), other);
        }
        Err(e) => {
            map.insert("error".into(), format!("{e:#}").into());
        }
    }
    serde_json::Value::Object(map)
}

fn print(
    ctx: &Ctx,
    results: Vec<(&Controller, Result<Box<dyn Emit>>)>,
    quiet_misses: bool,
) -> Result<bool> {
    let any_ok = results.iter().any(|(_, r)| r.is_ok());
    let shown: Vec<(&Controller, Result<Box<dyn Emit>>)> = results
        .into_iter()
        .filter(|(_, r)| !(quiet_misses && any_ok && r.is_err()))
        .collect();
    let failed = shown.iter().any(|(_, r)| r.is_err());
    match ctx.format {
        Format::Json => {
            let docs: Vec<serde_json::Value> = shown
                .into_iter()
                .map(|(c, r)| envelope(c, r.and_then(|e| e.json())))
                .collect();
            println!("{}", serde_json::to_string_pretty(&docs)?);
        }
        Format::Text => {
            let headed = shown.len() > 1;
            let mut first = true;
            for (c, r) in shown {
                match r {
                    Ok(e) => {
                        if headed {
                            if !first {
                                println!();
                            }
                            println!("{}", c.label());
                            println!();
                        }
                        first = false;
                        print!("{}", e.text());
                    }
                    Err(e) if headed => eprintln!("error: controller {}: {e:#}", c.id),
                    Err(e) => eprintln!("error: {e:#}"),
                }
            }
        }
    }
    Ok(!failed)
}

fn run(cli: Cli, what: &str, ctx: &Ctx) -> Result<bool> {
    let ctrls = controllers::discover(&ctx.sysfs);
    let command = cli.command.unwrap_or(Command::Controller {
        id: None,
        action: None,
    });
    let hint = command.controller_hint().map_err(|e| anyhow!(e))?;
    let wanted = match (hint, cli.controller) {
        (Some(a), Some(b)) if a != b => bail!("-c {b} and the ID point at different controllers"),
        (a, b) => a.or(b),
    };
    if let Command::Controller {
        id: None,
        action: None,
    } = command
        && wanted.is_none()
    {
        let list = controllers::list(&ctrls);
        let ok = list.controllers.iter().all(|c| c.error.is_none());
        match ctx.format {
            Format::Json => println!("{}", serde_json::to_string_pretty(&list.json()?)?),
            Format::Text => print!("{}", list.text()),
        }
        return Ok(ok);
    }
    if let Some((noun, list)) = command.missing_object() {
        if ctx.format == Format::Text {
            let targets = pick(&ctrls, wanted, &list, noun)?;
            let results = targets
                .into_iter()
                .map(|c| (c, execute(c, &list, ctx)))
                .collect();
            print(ctx, results, false)?;
            println!();
        }
        let rest = what.strip_prefix(noun).unwrap_or(what).trim();
        bail!(
            "{what} needs a {noun}, run it as sasctl {noun} <ID> {rest} with an ID from sasctl {noun}"
        );
    }
    let targets = pick(&ctrls, wanted, &command, what)?;
    let results = targets
        .into_iter()
        .map(|c| (c, execute(c, &command, ctx)))
        .collect();
    print(ctx, results, command.scope() == Scope::Find)
}

fn main() -> ExitCode {
    let matches = Cli::command().get_matches();
    let what = command_path(&matches);
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };
    let ctx = Ctx {
        format: if cli.json { Format::Json } else { Format::Text },
        yes: cli.yes,
        interactive: std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
        sysfs: cli.sysfs.clone(),
    };
    let what = if what.is_empty() {
        "sasctl".to_string()
    } else {
        what
    };
    match run(cli, &what, &ctx) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

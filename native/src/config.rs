use crate::Compiler;
use fnv::FnvHashMap;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, env, path::PathBuf, sync::Arc};
use swc::{
    atoms::JsWord,
    common::{chain, FileName},
    ecmascript::{
        ast::{Expr, ModuleItem, Stmt},
        parser::{Parser, Session as ParseSess, SourceFileInput, Syntax},
        transforms::{
            compat, fixer, helpers, hygiene, pass::Pass, react, simplifier, typescript,
            InlineGlobals,
        },
    },
};

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Options {
    #[serde(flatten, default)]
    pub config: Option<Config>,

    #[serde(default = "default_cwd")]
    pub cwd: PathBuf,

    #[serde(default)]
    pub caller: Option<CallerOptions>,

    #[serde(default)]
    pub filename: String,

    #[serde(default)]
    pub config_file: Option<ConfigFile>,

    #[serde(default)]
    pub root: Option<PathBuf>,

    #[serde(default)]
    pub root_mode: RootMode,

    #[serde(default = "default_swcrc")]
    pub swcrc: bool,

    #[serde(default)]
    pub swcrc_roots: Option<PathBuf>,

    #[serde(default = "default_env_name")]
    pub env_name: String,
}

impl Options {
    pub fn build(&self, c: &Compiler, config: Option<Config>) -> BuiltConfig {
        // let config = config.unwrap_or_else(|| Default::default());
        let mut config = config.unwrap_or_else(|| Default::default());

        if let Some(ref c) = self.config {
            if let Some(ref s) = c.jsc.transform {
                config.jsc.transform = Some(s.clone());
            }
            if let Some(ref t) = c.jsc.transform {
                config.jsc.transform = Some(t.clone());
            }
        }

        let helpers = Arc::new(helpers::Helpers::default());
        let JscConfig { transform, syntax } = config.jsc;
        let syntax = syntax.unwrap_or_default();
        let transform = transform.unwrap_or_default();

        let optimizer = transform.optimizer;
        let enable_optimizer = optimizer.is_some();
        let pass = if let Some(opts) =
            optimizer.map(|o| o.globals.unwrap_or_else(|| Default::default()))
        {
            opts.build(c)
        } else {
            GlobalPassOption::default().build(c)
        };

        let pass = chain!(
            pass,
            // handle jsx
            react::react(c.cm.clone(), transform.react, helpers.clone(),),
            simplifier(enable_optimizer),
            typescript::strip(),
            compat::es2018(&helpers),
            compat::es2017(&helpers),
            compat::es2016(),
            compat::es2015(&helpers),
            compat::es3(),
            hygiene(),
            fixer(),
            helpers::InjectHelpers {
                cm: c.cm.clone(),
                helpers: helpers.clone(),
            }
        );

        BuiltConfig {
            pass: box pass,
            syntax,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RootMode {
    #[serde(rename = "root")]
    Root,
    #[serde(rename = "upward")]
    Upward,
    #[serde(rename = "upward-optional")]
    UpwardOptional,
}

impl Default for RootMode {
    fn default() -> Self {
        RootMode::Root
    }
}
const fn default_swcrc() -> bool {
    true
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConfigFile {
    Bool(bool),
    Str(String),
}

impl Default for ConfigFile {
    fn default() -> Self {
        ConfigFile::Bool(true)
    }
}

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallerOptions {
    pub name: String,
}

fn default_cwd() -> PathBuf {
    ::std::env::current_dir().unwrap()
}

/// `.swcrc` file
#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct Config {
    #[serde(default)]
    pub jsc: JscConfig,
}

/// One `BuiltConfig` per a directory with swcrc
pub(crate) struct BuiltConfig {
    pub pass: Box<dyn Pass>,
    pub syntax: Syntax,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct JscConfig {
    #[serde(rename = "parser", default)]
    pub syntax: Option<Syntax>,
    #[serde(default)]
    pub transform: Option<TrnasformConfig>,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct TrnasformConfig {
    #[serde(default)]
    pub react: react::Options,
    #[serde(default)]
    pub optimizer: Option<OptimizerConfig>,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct OptimizerConfig {
    #[serde(default)]
    pub globals: Option<GlobalPassOption>,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct GlobalPassOption {
    #[serde(default)]
    pub vars: FnvHashMap<String, String>,
}

impl GlobalPassOption {
    pub fn build(self, c: &Compiler) -> InlineGlobals {
        fn mk_map(
            c: &Compiler,
            values: impl Iterator<Item = (String, String)>,
            is_env: bool,
        ) -> HashMap<JsWord, Expr> {
            let mut m = HashMap::new();

            for (k, v) in values {
                let v = if is_env {
                    format!("'{}'", v)
                } else {
                    (*v).into()
                };
                let v_str = v.clone();
                let fm =
                    c.cm.new_source_file(FileName::Custom(format!("GLOBAL.{}", k)), v);
                let session = ParseSess {
                    handler: &c.handler,
                };
                let mut module = Parser::new(
                    session,
                    Syntax::Es(Default::default()),
                    SourceFileInput::from(&*fm),
                )
                .parse_module()
                .map_err(|e| {
                    e.emit();
                    ()
                })
                .unwrap_or_else(|()| {
                    panic!(
                        "failed to parse global variable {}=`{}` as module",
                        k, v_str
                    )
                });

                let expr = match module.body.pop().unwrap() {
                    ModuleItem::Stmt(Stmt::Expr(box expr)) => expr,
                    _ => panic!("{} is not a valid expression", v_str),
                };

                m.insert((*k).into(), expr);
            }

            m
        }

        InlineGlobals {
            globals: mk_map(c, self.vars.into_iter(), false),
            envs: mk_map(c, env::vars(), true),
        }
    }
}

fn default_env_name() -> String {
    match env::var("SWC_ENV") {
        Ok(v) => return v,
        Err(_) => {}
    }

    match env::var("NODE_ENV") {
        Ok(v) => return v,
        Err(_) => return "development".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecmascript::parser::TsConfig;

    #[test]
    fn test() {
        println!(
            "{}",
            serde_json::to_string_pretty(&Config {
                jsc: JscConfig {
                    syntax: Syntax::Typescript(TsConfig {
                        ..Default::default()
                    }),
                    transform: TrnasformConfig {
                        react: react::Options {
                            ..Default::default()
                        },
                        optimizer: Some(OptimizerConfig {
                            globals: Some(GlobalPassOption {
                                vars: {
                                    let mut map = FnvHashMap::default();
                                    map.insert("__DEBUG__".into(), "true".into());
                                    map
                                }
                            }),
                        })
                    }
                },
            })
            .unwrap()
        );
    }

}

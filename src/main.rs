#![allow(dead_code, reason = "")]

use std::path::PathBuf;
use structopt::StructOpt;

use wasmtime::*;
use wasmtime_wasi::p1;

const STUBS: &'static str = include_str!("../lib/weval-stubs.wat");

#[derive(Clone, Debug, StructOpt)]
pub enum Command {
    /// Partially evaluate a Wasm module, optionally wizening first.
    Weval {
        /// The input Wasm module.
        #[structopt(short = "i")]
        input_module: PathBuf,

        /// The output Wasm module.
        #[structopt(short = "o")]
        output_module: PathBuf,

        /// Whether to Wizen the module first.
        #[structopt(short = "w")]
        wizen: bool,

        /// Preopened directories during Wizening, if any.
        #[structopt(long = "dir")]
        preopens: Vec<PathBuf>,

        /// Name of the Wizer initialization function to call.
        #[structopt(long = "init-func", default_value = "wizer-initialize")]
        init_func: String,

        /// Cache file to use.
        #[structopt(long = "cache")]
        cache: Option<PathBuf>,

        /// Read-only cache file to query.
        #[structopt(long = "cache-ro")]
        cache_ro: Option<PathBuf>,

        /// Show stats on specialization code size.
        #[structopt(long = "show-stats")]
        show_stats: bool,

        /// Output IR for generic and specialized functions to files in a directory.
        #[structopt(long = "output-ir")]
        output_ir: Option<PathBuf>,

        /// Emit verbose progress messages.
        #[structopt(short = "v", long = "verbose")]
        verbose: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let cmd = Command::from_args();

    match cmd {
        Command::Weval {
            input_module,
            output_module,
            wizen,
            preopens,
            init_func,
            cache,
            cache_ro,
            show_stats,
            output_ir,
            verbose,
        } => tokio::runtime::Runtime::new()?.block_on(async move {
            let input_module = tokio::fs::read(input_module).await?;
            let bytes = weval::weval(
                input_module,
                wizen.then(|| async |bytes| wizen_impl(bytes, preopens, init_func).await),
                cache,
                cache_ro,
                show_stats,
                output_ir,
                verbose,
            )
            .await?;
            tokio::fs::write(output_module, bytes).await?;
            anyhow::Ok(())
        }),
    }
}

async fn wizen_impl(
    raw_bytes: Vec<u8>,
    preopens: Vec<PathBuf>,
    init_func: String,
) -> anyhow::Result<Vec<u8>> {
    let mut config = Config::new();
    config.async_support(true);
    config.wasm_bulk_memory(true);
    let engine = Engine::new(&config)?;

    let mut wasi_ctx = wasmtime_wasi::WasiCtxBuilder::new();
    wasi_ctx.inherit_stdio();
    wasi_ctx.inherit_env();
    for preopen in &preopens {
        wasi_ctx.preopened_dir(
            preopen,
            preopen.to_str().unwrap_or("."),
            wasmtime_wasi::DirPerms::all(),
            wasmtime_wasi::FilePerms::all(),
        )?;
    }

    let mut store = Store::new(&engine, wasi_ctx.build_p1());
    let mut linker = Linker::new(&engine);
    p1::add_to_linker_async(&mut linker, |cx| cx)?;

    // Preload the weval stubs module.
    let stubs_module = Module::new(&engine, STUBS)?;
    let stubs_instance = linker.instantiate_async(&mut store, &stubs_module).await?;
    linker.instance(&mut store, "weval", stubs_instance)?;

    let mut wizer = wasmtime_wizer::Wizer::new();
    wizer.init_func(&init_func);
    wizer.func_rename("_start", "wizer.resume");

    wizer
        .run(&mut store, &raw_bytes, async |store, module| {
            linker.define_unknown_imports_as_traps(module)?;
            linker.instantiate_async(store, module).await
        })
        .await
}

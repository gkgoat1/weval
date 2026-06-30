use std::path::PathBuf;

mod cache;
mod constant_offsets;
mod dce;
mod directive;
mod escape;
mod eval;
mod filter;
mod image;
mod intrinsics;
mod liveness;
mod state;
mod stats;
mod value;


/// Weval a wasm.
pub async fn weval(
    input_module: Vec<u8>,
    do_wizen: Option<impl AsyncFnOnce(Vec<u8>) -> anyhow::Result<Vec<u8>>>,
    cache: Option<PathBuf>,
    cache_ro: Option<PathBuf>,
    show_stats: bool,
    output_ir: Option<PathBuf>,
    verbose: bool,
) -> anyhow::Result<Vec<u8>> {
    if verbose {
        eprintln!("Reading raw module bytes...");
    }
    let raw_bytes = input_module;

    // Compute a hash of the original module so we can cache results
    // keyed on that hash (and weval request arg strings).
    let input_hash = cache::compute_hash(&raw_bytes[..]);

    // Open the cache and read-only cache, if any.
    let cache = cache::Cache::open(
        cache.as_ref().map(|p| p.as_path()),
        cache_ro.as_ref().map(|p| p.as_path()),
        input_hash,
    )?;

    // Optionally, Wizen the module first.
    let module_bytes = if let Some(do_wizen) = do_wizen {
        if verbose {
            eprintln!("Wizening the module with its input...");
        }
        do_wizen(raw_bytes).await?
    } else {
        raw_bytes
    };

    // Load module.
    if verbose {
        eprintln!("Parsing the module...");
    }
    let mut frontend_opts = waffle::FrontendOptions::default();
    frontend_opts.debug = true;
    let module = waffle::Module::from_wasm_bytes(&module_bytes[..], &frontend_opts)?;

    // Build module image.
    if verbose {
        eprintln!("Building memory image...");
    }
    let mut im = image::build_image(&module, None)?;

    // Collect directives.
    let directives = directive::collect(&module, &mut im)?;
    log::debug!("Directives: {directives:?}");

    // Make sure IR output directory exists.
    if let Some(dir) = &output_ir {
        std::fs::create_dir_all(dir)?;
    }

    // Partially evaluate.
    if verbose {
        eprintln!("Specializing functions...");
    }
    let progress = if verbose {
        Some(indicatif::ProgressBar::new(0))
    } else {
        None
    };
    let mut result = eval::partially_evaluate(
        module,
        &mut im,
        &directives[..],
        progress,
        output_ir,
        &cache,
    )?;

    // Update memories in module.
    if verbose {
        eprintln!("Updatimg memory image...");
    }
    image::update(&mut result.module, &im);

    log::debug!("Final module:\n{}", result.module.display());

    if show_stats {
        for stats in result.stats {
            eprintln!(
                "Function {}: {} blocks, {} insts)",
                stats.generic, stats.generic_blocks, stats.generic_insts,
            );
            eprintln!(
                "   specialized ({} times): {} blocks, {} insts",
                stats.specializations, stats.specialized_blocks, stats.specialized_insts
            );
            eprintln!(
                "   virtstack: {} reads ({} mem), {} writes ({} mem)",
                stats.virtstack_reads,
                stats.virtstack_reads_mem,
                stats.virtstack_writes,
                stats.virtstack_writes_mem
            );
            eprintln!(
                "   locals: {} reads ({} mem), {} writes ({} mem)",
                stats.local_reads,
                stats.local_reads_mem,
                stats.local_writes,
                stats.local_writes_mem
            );
            eprintln!(
                "   live values at block starts: {} ({} per block)",
                stats.live_value_at_block_start,
                (stats.live_value_at_block_start as f64) / (stats.specialized_blocks as f64),
            );
        }
    }

    if verbose {
        eprintln!("Serializing back to binary form...");
    }
    let bytes = result.module.to_wasm_bytes()?;

    if verbose {
        eprintln!("Performing post-filter pass to remove intrinsics...");
    }
    let bytes = filter::filter(&bytes[..])?;


    if verbose {
        eprintln!("Done.");
    }
    Ok(bytes)
}

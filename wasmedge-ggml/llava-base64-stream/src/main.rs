use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::io::{self, Write};
use wasi_nn::{self, GraphExecutionContext};

fn read_input() -> String {
    loop {
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .expect("Failed to read line");
        if !answer.is_empty() && answer != "\n" && answer != "\r\n" {
            return answer.trim().to_string();
        }
    }
}

fn get_options_from_env() -> HashMap<&'static str, Value> {
    let mut options = HashMap::new();

    // Required parameters for llava
    if let Ok(val) = env::var("mmproj") {
        options.insert("mmproj", Value::from(val.as_str()));
    } else {
        eprintln!("Failed to get mmproj model.");
        std::process::exit(1);
    }

    // Optional parameters
    if let Ok(val) = env::var("enable_log") {
        options.insert("enable-log", serde_json::from_str(val.as_str()).unwrap());
    } else {
        options.insert("enable-log", Value::from(false));
    }
    if let Ok(val) = env::var("ctx_size") {
        options.insert("ctx-size", serde_json::from_str(val.as_str()).unwrap());
    } else {
        options.insert("ctx-size", Value::from(2048));
    }
    if let Ok(val) = env::var("n_gpu_layers") {
        options.insert("n-gpu-layers", serde_json::from_str(val.as_str()).unwrap());
    } else {
        options.insert("n-gpu-layers", Value::from(0));
    }
    options
}

fn set_data_to_context(
    context: &mut GraphExecutionContext,
    data: Vec<u8>,
) -> Result<(), wasi_nn::Error> {
    context.set_input(0, wasi_nn::TensorType::U8, &[1], &data)
}

fn get_data_from_context(context: &GraphExecutionContext, index: usize, is_single: bool) -> String {
    // Preserve for 4096 tokens with average token length 6
    const MAX_OUTPUT_BUFFER_SIZE: usize = 4096 * 6;
    let mut output_buffer = vec![0u8; MAX_OUTPUT_BUFFER_SIZE];
    let mut output_size = if is_single {
        context
            .get_output_single(index, &mut output_buffer)
            .expect("Failed to get single output")
    } else {
        context
            .get_output(index, &mut output_buffer)
            .expect("Failed to get output")
    };
    output_size = std::cmp::min(MAX_OUTPUT_BUFFER_SIZE, output_size);

    return String::from_utf8_lossy(&output_buffer[..output_size]).to_string();
}

fn get_output_from_context(context: &GraphExecutionContext) -> String {
    get_data_from_context(context, 0, false)
}

fn get_single_output_from_context(context: &GraphExecutionContext) -> String {
    get_data_from_context(context, 0, true)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let model_name: &str = &args[1];

    // Set options for the graph. Check our README for more details:
    // https://github.com/second-state/WasmEdge-WASINN-examples/tree/master/wasmedge-ggml#parameters
    let options = get_options_from_env();

    // Create graph and initialize context.
    let graph =
        wasi_nn::GraphBuilder::new(wasi_nn::GraphEncoding::Ggml, wasi_nn::ExecutionTarget::AUTO)
            .config(serde_json::to_string(&options).expect("Failed to serialize options"))
            .build_from_cache(model_name)
            .expect("Failed to build graph");
    let mut context = graph
        .init_execution_context()
        .expect("Failed to init context");

    // If there is a third argument, use it as the prompt and enter non-interactive mode.
    // This is mainly for the CI workflow.
    if args.len() >= 3 {
        let prompt = &args[2];
        println!("Prompt:\n{}", prompt);
        let tensor_data = prompt.as_bytes().to_vec();
        context
            .set_input(0, wasi_nn::TensorType::U8, &[1], &tensor_data)
            .expect("Failed to set input");
        println!("Response:");
        context.compute().expect("Failed to compute");
        let output = get_output_from_context(&context);
        println!("{}", output.trim());
        std::process::exit(0);
    }

    let mut saved_prompt = String::new();
    let system_prompt = String::from("You are a helpful, respectful and honest assistant. Always answer as short as possible, while being safe." );
    // This image is converted from https://upload.wikimedia.org/wikipedia/commons/2/2f/Blackberryraspberrystrawberry.jpg
    let image_tag = format!(
        "<img src=\"data:image/jpeg;base64,{}\">",
        image_base64_bytes
    );

    loop {
        println!("USER:");
        let input = read_input();

        // llava chat format is "<system_prompt>\nUSER:<image_embeddings>\n<textual_prompt>\nASSISTANT:"
        if saved_prompt.is_empty() {
            saved_prompt = format!(
                "{}\nUSER:{}\n{}\nASSISTANT:",
                system_prompt, image_tag, input
            );
        } else {
            saved_prompt = format!("{}\nUSER: {}\nASSISTANT:", saved_prompt, input);
        }

        // Set prompt to the input tensor.
        set_data_to_context(&mut context, saved_prompt.as_bytes().to_vec())
            .expect("Failed to set input");

        // Execute the inference.
        let mut output = String::new();
        let mut reset_prompt = false;
        println!("ASSISTANT:");
        loop {
            match context.compute_single() {
                Ok(_) => (),
                Err(wasi_nn::Error::BackendError(wasi_nn::BackendError::EndOfSequence)) => {
                    break;
                }
                Err(wasi_nn::Error::BackendError(wasi_nn::BackendError::ContextFull)) => {
                    println!("\n[INFO] Context full, we'll reset the context and continue.");
                    reset_prompt = true;
                    break;
                }
                Err(wasi_nn::Error::BackendError(wasi_nn::BackendError::PromptTooLong)) => {
                    println!("\n[INFO] Prompt too long, we'll reset the context and continue.");
                    reset_prompt = true;
                    break;
                }
                Err(err) => {
                    println!("\n[ERROR] {}", err);
                    break;
                }
            }
            // Retrieve the single output token and print it.
            let token = get_single_output_from_context(&context);
            print!("{}", token);
            io::stdout().flush().unwrap();
            output += &token;
        }
        println!();

        // Update the saved prompt.
        if reset_prompt {
            saved_prompt.clear();
        } else {
            output = output.trim().to_string();
            saved_prompt = format!("{} {}", saved_prompt, output);
        }

        // Delete the context in compute_single mode.
        context.fini_single().unwrap();
    }
}
use clap::{Parser, Subcommand};
use colored::*;
use console::{style, Term};
use dialoguer::{theme::ColorfulTheme, Confirm};
use indicatif::{ProgressBar, ProgressStyle};
use log::info;
use openai_api_rust::chat::*;
use openai_api_rust::*;
use std::time::Duration;
use thiserror::Error;

const MODEL_NAME: &str = "gpt-4";

#[derive(Error, Debug)]
enum CommitauraError {
    #[error("No staged changes detected")]
    NoStagedChanges,
    #[error("Git operation failed: {0}")]
    GitOperationFailed(String),
    #[error("API request failed: {0}")]
    ApiRequestFailed(String),
    #[error("Environment variable not set: {0}")]
    EnvVarNotSet(String),
    #[error("OpenAI API error: {0}")]
    OpenAIError(String),
    #[error("Template error: {0}")]
    TemplateError(#[from] indicatif::style::TemplateError),
    #[error("Dialoguer error: {0}")]
    DialoguerError(#[from] dialoguer::Error),
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
}

// Removed redundant implementation

#[derive(Parser)]
#[command(name = "Commitaura")]
#[command(about = "Intelligent Git Commit Assistant", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Automatically generate commit message and commit
    Commit,
}

fn main() -> Result<(), CommitauraError> {
    env_logger::init();
    dotenv::dotenv().ok();

    let auth = Auth::from_env()
        .map_err(|_| CommitauraError::EnvVarNotSet("OPENAI_API_KEY".to_string()))?;
    let openai = OpenAI::new(auth, "https://api.openai.com/v1/");

    let cli = Cli::parse();
    let term = Term::stdout();

    match cli.command {
        Some(Commands::Commit) | None => handle_commit(&openai, &term)?,
    }
    Ok(())
}

fn handle_commit(openai: &OpenAI, term: &Term) -> Result<(), CommitauraError> {
    term.clear_screen()?;
    println!("{}", style("Commit Changes").bold().underlined());
    println!();

    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::default_spinner().template("{spinner} {msg}")?);

    pb.set_message("Checking staged changes...");
    check_staged_changes()?;

    pb.set_message("Fetching recent commit messages...");
    let last_commits = get_last_commit_messages()?;
    pb.finish_and_clear();

    display_commit_messages(&last_commits);

    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::default_spinner().template("{spinner} {msg}")?);
    pb.set_message("Generating commit message from OpenAI...");
    let commit_message = generate_commit_message(openai, &last_commits)?;

    pb.finish_and_clear();

    println!("Generated commit message:");
    println!("{}", style(&commit_message).green().bold());
    println!();

    if Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Do you want to proceed with this commit message?")
        .default(true)
        .interact()?
    {
        pb.set_message("Committing changes...");
        pb.enable_steady_tick(Duration::from_millis(100));
        perform_git_commit(&commit_message)?;
        pb.finish_with_message("Commit successful!");
    } else {
        println!("Commit cancelled.");
    }

    Ok(())
}

fn check_staged_changes() -> Result<(), CommitauraError> {
    let output = std::process::Command::new("git")
        .args(&["diff", "--staged", "--quiet"])
        .status()
        .map_err(|e| CommitauraError::GitOperationFailed(e.to_string()))?;

    if output.success() {
        Err(CommitauraError::NoStagedChanges)
    } else {
        Ok(())
    }
}

fn perform_git_commit(message: &str) -> Result<(), CommitauraError> {
    let status = std::process::Command::new("git")
        .args(&["commit", "-m", message])
        .status()
        .map_err(|e| CommitauraError::GitOperationFailed(e.to_string()))?;

    if status.success() {
        Ok(())
    } else {
        Err(CommitauraError::GitOperationFailed(
            "Git commit failed".to_string(),
        ))
    }
}

fn get_last_commit_messages() -> Result<String, CommitauraError> {
    let output = std::process::Command::new("git")
        .args(&["log", "-5", "--pretty=format:%s"])
        .output()
        .map_err(|e| CommitauraError::GitOperationFailed(e.to_string()))?;

    String::from_utf8(output.stdout).map_err(|e| CommitauraError::GitOperationFailed(e.to_string()))
}

fn generate_commit_message(openai: &OpenAI, last_commits: &str) -> Result<String, CommitauraError> {
    let diff_output = std::process::Command::new("git")
        .args(&["diff", "--staged"])
        .output()
        .map_err(|e| CommitauraError::GitOperationFailed(e.to_string()))?;

    let diff = String::from_utf8(diff_output.stdout)
        .map_err(|e| CommitauraError::GitOperationFailed(e.to_string()))?;

    if diff.trim().is_empty() {
        return Err(CommitauraError::NoStagedChanges);
    }

    let body = ChatBody {
        model: MODEL_NAME.to_string(),
        max_tokens: Some(100),
        temperature: Some(0.7),
        top_p: Some(1.0),
        n: Some(1),
        stream: Some(false),
        stop: None,
        presence_penalty: None,
        frequency_penalty: None,
        logit_bias: None,
        user: None,
        messages: vec![
            Message {
                role: Role::System,
                content: "You are a helpful assistant that generates concise and meaningful Git commit messages.".to_string(),
            },
            Message {
                role: Role::User,
                content: format!(
                    "Write a concise and meaningful Git commit message based on the following changes (do not include any other text other than the commit message). Be extremely specific. Do not be vague. Consider the context of the last 5 commit messages:\n\nLast 5 commit messages:\n{}\n\nCurrent changes:\n{}",
                    last_commits, diff
                ),
            },
        ],
    };

    let rs = openai
        .chat_completion_create(&body)
        .map_err(|e| CommitauraError::OpenAIError(e.to_string()))?;

    let choice = rs.choices;
    let message = &choice[0]
        .message
        .as_ref()
        .ok_or(CommitauraError::ApiRequestFailed(
            "No message in API response".to_string(),
        ))?;
    let commit_message = message.content.trim().to_string();

    if commit_message.is_empty() {
        Err(CommitauraError::ApiRequestFailed(
            "Received empty commit message from LLM.".to_string(),
        ))
    } else {
        info!("Generated commit message: {}", commit_message);
        Ok(commit_message)
    }
}

fn display_commit_messages(commits: &str) {
    println!("{}", "📜 Recent Commit Messages:".bold().bright_blue());
    println!("{}", "─".repeat(40).bright_blue());
    for (i, message) in commits.lines().enumerate() {
        println!(
            "{} {}",
            format!("{}.", i + 1).bright_yellow(),
            message.bright_white()
        );
    }
    println!("{}", "─".repeat(40).bright_blue());
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_staged_changes() {
        // This test assumes that there are no staged changes in the test environment
        assert!(matches!(
            check_staged_changes(),
            Err(CommitauraError::NoStagedChanges)
        ));
    }

    #[test]
    fn test_generate_commit_message() {
        // Mock the OpenAI client and test the generate_commit_message function
        // This is a placeholder and should be implemented with proper mocking
    }
}

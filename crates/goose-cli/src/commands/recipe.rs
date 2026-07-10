use anyhow::Result;
use console::style;
use goose::recipe::validate_recipe::validate_recipe_template_from_file;

use crate::recipes::github_recipe::RecipeSource;
use crate::recipes::search_recipe::{list_available_recipes, load_recipe_file};

pub fn handle_validate(recipe_name: &str) -> Result<()> {
    // Load and validate the recipe file
    let recipe_file = load_recipe_file(recipe_name)?;
    validate_recipe_template_from_file(&recipe_file).map_err(|err| {
        anyhow::anyhow!(
            "{} recipe file is invalid: {}",
            style("✗").red().bold(),
            err
        )
    })?;
    println!("{} recipe file is valid", style("✓").green().bold());
    Ok(())
}

pub fn handle_list(format: &str, verbose: bool) -> Result<()> {
    let recipes = match list_available_recipes() {
        Ok(recipes) => recipes,
        Err(e) => {
            return Err(anyhow::anyhow!("Failed to list recipes: {}", e));
        }
    };

    match format {
        "json" => {
            println!("{}", serde_json::to_string(&recipes)?);
        }
        _ => {
            if recipes.is_empty() {
                println!("No recipes found");
                return Ok(());
            } else {
                println!("Available recipes:");
                for recipe in recipes {
                    let source_info = match recipe.source {
                        RecipeSource::Local => format!("local: {}", recipe.path),
                        RecipeSource::GitHub => format!("github: {}", recipe.path),
                    };

                    let description = if let Some(desc) = &recipe.description {
                        if desc.is_empty() {
                            "(none)"
                        } else {
                            desc
                        }
                    } else {
                        "(none)"
                    };

                    let output = format!("{} - {} - {}", recipe.name, description, source_info);
                    if verbose {
                        println!("  {}", output);
                        if let Some(title) = &recipe.title {
                            println!("    Title: {}", title);
                        }
                        println!("    Path: {}", recipe.path);
                    } else {
                        println!("{}", output);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_recipe_file(dir: &TempDir, filename: &str, content: &str) -> String {
        let file_path = dir.path().join(filename);
        fs::write(&file_path, content).expect("Failed to write test recipe file");
        file_path.to_string_lossy().into_owned()
    }

    const VALID_RECIPE_CONTENT: &str = r#"
title: "Test Recipe with Valid JSON Schema"
description: "A test recipe with valid JSON schema"
prompt: "Test prompt content"
instructions: "Test instructions"
response:
  json_schema:
    type: object
    properties:
      result:
        type: string
        description: "The result"
      count:
        type: number
        description: "A count value"
    required:
      - result
"#;

    const INVALID_RECIPE_CONTENT: &str = r#"
title: "Test Recipe"
description: "A test recipe for validation"
prompt: "Test prompt content {{ name }}"
instructions: "Test instructions"
"#;

    #[test]
    fn test_handle_validation_valid_recipe() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let recipe_path =
            create_test_recipe_file(&temp_dir, "test_recipe.yaml", VALID_RECIPE_CONTENT);

        let result = handle_validate(&recipe_path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_handle_validation_invalid_recipe() {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let recipe_path =
            create_test_recipe_file(&temp_dir, "test_recipe.yaml", INVALID_RECIPE_CONTENT);
        let result = handle_validate(&recipe_path);
        assert!(result.is_err());
    }
}

#![warn(clippy::all, clippy::pedantic)]

use anyhow::anyhow;
use colored::Colorize;
use itertools::Itertools;
use reqwest::{
    blocking::Client,
    header::{HeaderMap, HeaderValue},
    Url,
};
use serde::Deserialize;
use std::{collections::HashMap, process};
use strum::IntoEnumIterator;
use text_io::read;

use ynabifier::config::{Builder as ConfigBuilder, Parser};

macro_rules! read_line {
    () => {
        read!("{}\n")
    };
}

trait ExpectExit<T, R> {
    fn expect_exit<F: FnOnce(R)>(self, exit_code: i32, f: F) -> T;
}

impl<T> ExpectExit<T, Option<T>> for Option<T> {
    fn expect_exit<F: FnOnce(Option<T>)>(self, exit_code: i32, f: F) -> T {
        if let Some(item) = self {
            item
        } else {
            f(None);
            process::exit(exit_code);
        }
    }
}

impl<T, R> ExpectExit<T, R> for Result<T, R> {
    fn expect_exit<F: FnOnce(R)>(self, exit_code: i32, f: F) -> T {
        match self {
            Ok(item) => item,
            Err(err) => {
                f(err);
                process::exit(exit_code);
            }
        }
    }
}

fn main() {
    eprintln!("Welcome to the YNABifier setup wizard. This will guide you through the steps needed to create a config.yml");

    let error_prefix = "Error".red();
    let personal_access_token = prompt_for_personal_access_token();
    let client_res = setup_client(&personal_access_token);
    let client = client_res.expect_exit(2, |err| {
        eprintln!("{error_prefix} failed to setup YNAB client: {err}");
    });

    let maybe_budget_id = prompt_for_budget_id(&client).expect_exit(2, |err| {
        eprintln!("{error_prefix} failed to get budget: {err}");
    });

    let budget_id = maybe_budget_id.expect_exit(3, |_| {
        // if there's no budget id, that's fine, that's because they chose not to have one.
        //  no need to print anything
    });

    let accounts = prompt_for_accounts(&client, &budget_id).expect_exit(4, |err| {
        eprintln!("{error_prefix} failed to get accounts: {err}");
    });

    let config =
        build_config(&personal_access_token, &budget_id, &accounts).expect_exit(5, |err| {
            eprintln!("{error_prefix} failed to build config: {err}");
        });

    eprintln!(
        "\n{}",
        "Configuration generated. You should place the YAML in config.yml and fill in your IMAP account details. Enjoy YNABifier!".green()
    );
    println!("{config}");
}

fn prompt_for_personal_access_token() -> String {
    eprintln!("Enter your YNAB personal access token");
    read_line!()
}

fn prompt_for_budget_id(client: &Client) -> anyhow::Result<Option<String>> {
    let budgets = find_budgets(client)?.data.budgets;
    if budgets.is_empty() {
        Err(anyhow!(
            "No budgets were found on your account. Please set up your account to use YNABifier",
        ))
    } else if budgets.len() == 1 {
        let budget = &budgets[0];
        Ok(confirm_single_choice(budget, "budget").map(|budget| budget.id.to_string()))
    } else {
        Ok(Some(choose_budget(&budgets)).map(|budget| budget.id.to_string()))
    }
}

fn prompt_for_accounts(
    client: &Client,
    budget_id: &str,
) -> anyhow::Result<Vec<(NamedItem, Parser)>> {
    #![allow(clippy::print_literal)]
    let accounts = find_accounts(client, budget_id)?.data.accounts;
    if accounts.is_empty() {
        eprintln!(
            "No accounts were found on your budget. Please add accounts to your budget to use YNABifier",
        );

        return Ok(Vec::new());
    }

    let accounts = {
        if accounts.len() == 1 {
            let account = &accounts[0];
            confirm_single_choice(account, "account")
                .map(|item| vec![item.clone()])
                .unwrap_or_default()
        } else {
            choose_accounts(&accounts).into_iter().cloned().collect()
        }
    };

    eprintln!(
        "{} {}\n",
        "You must now select an email parser for your account(s). This will be used to read transactions from your emails.",
        "If your provider is not here, it will need to be added before you can use this account with YNABifier");
    let account_parsers = accounts
        .into_iter()
        .map(|account| {
            let parser = choose_parser_for_account(&account);
            (account, parser)
        })
        .collect();

    Ok(account_parsers)
}

fn confirm_single_choice<'a>(choice: &'a NamedItem, item_type: &str) -> Option<&'a NamedItem> {
    eprintln!("One {item_type} was found on your account.");
    eprintln!("Name: {}", choice.name);
    eprintln!("ID: {}", choice.id);
    loop {
        eprint!("\nWould you like to use this {item_type}? [Y/n]: ");
        let selection: String = read_line!();
        match selection.to_lowercase().as_ref() {
            "y" => return Some(choice),
            "n" => return None,
            _ => eprintln!("Invalid choice"),
        }
    }
}

fn choose_budget(budgets: &[NamedItem]) -> &NamedItem {
    eprintln!("Multiple budgets were found on your account. Select the one you would like to use");
    for (i, budget) in budgets.iter().enumerate() {
        let numeric_str = format!("{}) ", i + 1);
        eprintln!("{numeric_str}Name: {}", budget.name);
        eprintln!("{}ID: {}\n", " ".repeat(numeric_str.len()), budget.id);
    }
    loop {
        eprint!("Which budget would you like? (1-{}): ", budgets.len());
        let selection: String = read_line!();
        match selection.parse::<usize>() {
            Ok(n) if n >= 1 && n <= budgets.len() => return &budgets[n - 1],
            Ok(_) => eprintln!("Invalid choice: choice out of bounds"),
            Err(err) => eprintln!("Invalid choice: {err}"),
        }
    }
}

fn choose_accounts(accounts: &[NamedItem]) -> Vec<&NamedItem> {
    eprintln!("Multiple budgets were found on your account. Select the one you would like to use");
    for (i, account) in accounts.iter().enumerate() {
        let numeric_str = format!("{}) ", i + 1);
        eprintln!("{numeric_str}Name: {}", account.name);
        eprintln!("{}ID: {}\n", " ".repeat(numeric_str.len()), account.id);
    }
    loop {
        eprint!(
            "Which accounts would you like? (1-{}; comma separated): ",
            accounts.len()
        );
        let selection_input: String = {
            let mut input: String = read_line!();
            input.retain(|c| !c.is_whitespace());
            input
        };

        let selections_res = selection_input
            .split(',')
            .map(|raw_idx| str::parse(raw_idx).map_err(Into::into))
            .map(|idx_res| {
                idx_res.and_then(|idx| {
                    if idx >= 1 && idx <= accounts.len() {
                        Ok(idx)
                    } else {
                        Err(anyhow!("choice '{idx}' out of bounds"))
                    }
                })
            })
            .collect::<Result<Vec<usize>, anyhow::Error>>();

        match selections_res {
            Ok(selections) => return selections.into_iter().map(|n| &accounts[n - 1]).collect(),
            Err(err) => eprintln!("Invalid choice: {err}"),
        }
    }
}

fn choose_parser_for_account(account: &NamedItem) -> Parser {
    let parsers = Parser::iter()
        .map(|parser| {
            let parser_name: &str = parser.into();

            (parser_name.to_string().to_lowercase(), parser)
        })
        .collect::<HashMap<_, _>>();

    let parser_name_str = parsers.keys().join(", ");

    loop {
        eprintln!(
            "Which email parser would you would you like to use for the account \"{}\"? ({}): ",
            account.name, parser_name_str,
        );

        let input: String = read_line!();
        match parsers.get(&input.to_lowercase()) {
            Some(&parser) => return parser,
            None => eprintln!("Invalid choice, must be one of {parser_name_str}"),
        }
    }
}

fn setup_client(personal_access_token: &str) -> anyhow::Result<Client> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {personal_access_token}"))?,
    );
    headers.insert("Content-Type", HeaderValue::from_static("application/json"));
    let client = Client::builder().default_headers(headers).build()?;

    Ok(client)
}

fn build_config(
    personal_access_token: &str,
    budget_id: &str,
    accounts: &[(NamedItem, Parser)],
) -> Result<String, anyhow::Error> {
    let mut builder = ConfigBuilder::new()
        .imap("imap.REPLACEME.com", "user@REPLACEME.com", "SUPER_SECRET")?
        .ynab(personal_access_token, budget_id)?;
    for (account, parser) in accounts {
        builder = builder.ynab_account(&account.id, *parser);
    }

    let config = builder.build()?;
    let config_yaml = serde_yaml::to_string(&config)?;

    Ok(config_yaml)
}

#[derive(Deserialize, Clone)]
struct YNABData<D> {
    data: D,
}

#[derive(Deserialize, Clone)]
struct Budgets {
    budgets: Vec<NamedItem>,
}

#[derive(Deserialize, Clone)]
struct Accounts {
    accounts: Vec<NamedItem>,
}

#[derive(Deserialize, Clone)]
struct NamedItem {
    id: String,
    name: String,
}

fn find_budgets(client: &Client) -> anyhow::Result<YNABData<Budgets>> {
    let request = client
        .get("https://api.youneedabudget.com/v1/budgets/")
        .build()?;
    let budgets = client
        .execute(request)?
        .error_for_status()?
        .json::<YNABData<Budgets>>()?;

    Ok(budgets)
}

fn find_accounts(client: &Client, budget_id: &str) -> anyhow::Result<YNABData<Accounts>> {
    let url = Url::parse("https://api.youneedabudget.com/v1/budgets/")?
        .join(&format!("{budget_id}/accounts/"))?;
    let request = client.get(url).build()?;
    let accounts = client
        .execute(request)?
        .error_for_status()?
        .json::<YNABData<Accounts>>()?;

    Ok(accounts)
}

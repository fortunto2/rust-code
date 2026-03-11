mod api;
mod cli;
mod models;

use anyhow::Result;
use clap::Parser;
use cli::Args;
use colored::Colorize;
use reqwest::Client;

fn weather_description(code: u8) -> &'static str {
    match code {
        0 => "Ясно ☀️",
        1..=3 => "Облачно ⛅",
        45 | 48 => "Туман 🌫️",
        51..=55 => "Морось 🌧️",
        61..=65 => "Дождь 🌧️",
        71..=75 => "Снег ❄️",
        80..=82 => "Ливень 🌧️",
        95..=99 => "Гроза ⛈️",
        _ => "Неизвестно ❓",
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let client = Client::new();

    println!("{} {}...", "Поиск города".cyan(), args.city.yellow());

    let location = api::get_coordinates(&client, &args.city).await?;

    let country = location.country.unwrap_or_else(|| "Неизвестно".to_string());
    println!(
        "{} {}, {} (lat: {}, lon: {})",
        "Найдено:".green(),
        location.name.bold(),
        country,
        location.latitude,
        location.longitude
    );

    let weather = api::get_weather(&client, location.latitude, location.longitude).await?;
    let current = weather.current_weather;

    let desc = weather_description(current.weathercode);

    println!("\n{}", "Текущая погода:".blue().bold());
    println!(
        "Температура: {} °C",
        current.temperature.to_string().yellow()
    );
    println!("Ветер: {} км/ч", current.windspeed.to_string().cyan());
    println!("Состояние: {}", desc);

    Ok(())
}

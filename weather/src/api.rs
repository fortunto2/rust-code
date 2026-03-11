use crate::models::{GeocodingResponse, Location, WeatherResponse};
use anyhow::{Context, Result};
use reqwest::Client;

const GEOCODING_API_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";
const WEATHER_API_URL: &str = "https://api.open-meteo.com/v1/forecast";

pub async fn get_coordinates(client: &Client, city: &str) -> Result<Location> {
    let url = format!(
        "{}?name={}&count=1&language=ru&format=json",
        GEOCODING_API_URL, city
    );
    let response = client
        .get(&url)
        .send()
        .await?
        .json::<GeocodingResponse>()
        .await?;

    response
        .results
        .and_then(|mut r| r.pop())
        .context(format!("Город '{}' не найден", city))
}

pub async fn get_weather(client: &Client, lat: f64, lon: f64) -> Result<WeatherResponse> {
    let url = format!(
        "{}?latitude={}&longitude={}&current_weather=true",
        WEATHER_API_URL, lat, lon
    );
    let response = client
        .get(&url)
        .send()
        .await?
        .json::<WeatherResponse>()
        .await?;
    Ok(response)
}

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct GeocodingResponse {
    pub results: Option<Vec<Location>>,
}

#[derive(Debug, Deserialize)]
pub struct Location {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub country: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WeatherResponse {
    pub current_weather: CurrentWeather,
}

#[derive(Debug, Deserialize)]
pub struct CurrentWeather {
    pub temperature: f64,
    pub windspeed: f64,
    pub weathercode: u8,
}

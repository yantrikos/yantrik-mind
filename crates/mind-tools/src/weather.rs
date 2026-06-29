//! weather — keyless current conditions + today's forecast via open-meteo (no API key, generous free
//! tier). Geocodes a place name → current weather + daily hi/lo. Output is a short, plain digest.

use async_trait::async_trait;

#[async_trait]
pub trait WeatherClient: Send + Sync {
    /// A one-line(ish) weather report for `place` (city/town name).
    async fn report(&self, place: &str) -> anyhow::Result<String>;
}

/// WMO weather-interpretation code → short description.
fn wmo(code: i64) -> &'static str {
    match code {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "fog",
        51 | 53 | 55 => "drizzle",
        56 | 57 => "freezing drizzle",
        61 | 63 | 65 => "rain",
        66 | 67 => "freezing rain",
        71 | 73 | 75 => "snow",
        77 => "snow grains",
        80 | 81 | 82 => "rain showers",
        85 | 86 => "snow showers",
        95 => "thunderstorm",
        96 | 99 => "thunderstorm with hail",
        _ => "—",
    }
}

/// Keyless open-meteo weather.
pub struct OpenMeteo;

impl OpenMeteo {
    pub fn new() -> Self {
        Self
    }
}

impl Default for OpenMeteo {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl WeatherClient for OpenMeteo {
    async fn report(&self, place: &str) -> anyhow::Result<String> {
        let place = place.trim().to_string();
        if place.is_empty() {
            anyhow::bail!("which place?");
        }
        tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            // 1) geocode the place name → lat/lon (+ canonical name/country)
            let geo: serde_json::Value = ureq::get("https://geocoding-api.open-meteo.com/v1/search")
                .timeout(std::time::Duration::from_secs(15))
                .query("name", &place)
                .query("count", "1")
                .call()?
                .into_json()?;
            let r = geo["results"].get(0).cloned().ok_or_else(|| anyhow::anyhow!("couldn't find a place called \"{place}\""))?;
            let (lat, lon) = (r["latitude"].as_f64().unwrap_or(0.0), r["longitude"].as_f64().unwrap_or(0.0));
            let name = r["name"].as_str().unwrap_or(&place).to_string();
            let country = r["country"].as_str().unwrap_or("").to_string();
            // 2) current conditions + today's hi/lo
            let w: serde_json::Value = ureq::get("https://api.open-meteo.com/v1/forecast")
                .timeout(std::time::Duration::from_secs(15))
                .query("latitude", &lat.to_string())
                .query("longitude", &lon.to_string())
                .query("current", "temperature_2m,relative_humidity_2m,apparent_temperature,weather_code,wind_speed_10m")
                .query("daily", "temperature_2m_max,temperature_2m_min,weather_code")
                .query("timezone", "auto")
                .query("forecast_days", "1")
                .call()?
                .into_json()?;
            let cur = &w["current"];
            let temp = cur["temperature_2m"].as_f64().unwrap_or(0.0);
            let feels = cur["apparent_temperature"].as_f64().unwrap_or(temp);
            let hum = cur["relative_humidity_2m"].as_f64().unwrap_or(0.0);
            let wind = cur["wind_speed_10m"].as_f64().unwrap_or(0.0);
            let desc = wmo(cur["weather_code"].as_i64().unwrap_or(-1));
            let hi = w["daily"]["temperature_2m_max"][0].as_f64();
            let lo = w["daily"]["temperature_2m_min"][0].as_f64();
            let place_lbl = if country.is_empty() { name } else { format!("{name}, {country}") };
            let mut out = format!("🌦 {place_lbl}: {desc}, {temp:.0}°C (feels {feels:.0}°), humidity {hum:.0}%, wind {wind:.0} km/h.");
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push_str(&format!(" Today {lo:.0}–{hi:.0}°C."));
            }
            Ok(out)
        })
        .await?
    }
}

/// Deterministic weather for tests.
pub struct ScriptedWeather {
    pub line: String,
}

impl ScriptedWeather {
    pub fn new(line: impl Into<String>) -> Self {
        Self { line: line.into() }
    }
}

#[async_trait]
impl WeatherClient for ScriptedWeather {
    async fn report(&self, _place: &str) -> anyhow::Result<String> {
        Ok(self.line.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wmo_codes() {
        assert_eq!(wmo(0), "clear sky");
        assert_eq!(wmo(95), "thunderstorm");
        assert_eq!(wmo(61), "rain");
    }
}

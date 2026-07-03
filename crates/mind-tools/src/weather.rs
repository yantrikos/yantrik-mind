//! weather — keyless current conditions + today's forecast via open-meteo (no API key, generous free
//! tier). Geocodes a place name → current weather + daily hi/lo. Output is a short, plain digest.

use async_trait::async_trait;

/// One day of forecast — enough to judge "is this a good outdoor-photos day".
#[derive(Debug, Clone)]
pub struct DayForecast {
    pub date: String,    // YYYY-MM-DD, local to the place
    pub weekday: String, // "Sat"
    pub desc: String,
    pub hi_f: f64,
    pub lo_f: f64,
    pub precip_prob: f64, // %
    pub wind_mph: f64,
    pub sunset: String, // "18:45" local — golden hour anchor
}

#[async_trait]
pub trait WeatherClient: Send + Sync {
    /// A one-line(ish) weather report for `place` (city/town name).
    async fn report(&self, place: &str) -> anyhow::Result<String>;

    /// Daily outlook for up to 16 days ahead (open-meteo's horizon). Imperial units.
    async fn daily_outlook(&self, _place: &str, _days: u32) -> anyhow::Result<Vec<DayForecast>> {
        anyhow::bail!("daily outlook not supported by this weather backend");
    }
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

    async fn daily_outlook(&self, place: &str, days: u32) -> anyhow::Result<Vec<DayForecast>> {
        let place = place.trim().to_string();
        if place.is_empty() {
            anyhow::bail!("which place?");
        }
        let days = days.clamp(1, 16);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<DayForecast>> {
            let geocode = |name: &str| -> anyhow::Result<serde_json::Value> {
                Ok(ureq::get("https://geocoding-api.open-meteo.com/v1/search")
                    .timeout(std::time::Duration::from_secs(15))
                    .query("name", name)
                    .query("count", "1")
                    .call()?
                    .into_json()?)
            };
            // "Centerton, Arkansas" may not match as a comma string — fall back to the city alone.
            let mut geo = geocode(&place)?;
            if geo["results"].get(0).is_none() {
                if let Some(prefix) = place.split(',').next() {
                    geo = geocode(prefix.trim())?;
                }
            }
            let r = geo["results"].get(0).cloned().ok_or_else(|| anyhow::anyhow!("couldn't find a place called \"{place}\""))?;
            let (lat, lon) = (r["latitude"].as_f64().unwrap_or(0.0), r["longitude"].as_f64().unwrap_or(0.0));
            let w: serde_json::Value = match ureq::get("https://api.open-meteo.com/v1/forecast")
                .timeout(std::time::Duration::from_secs(15))
                .query("latitude", &lat.to_string())
                .query("longitude", &lon.to_string())
                .query("daily", "temperature_2m_max,temperature_2m_min,weather_code,precipitation_probability_max,wind_speed_10m_max,sunset")
                .query("timezone", "auto")
                .query("temperature_unit", "fahrenheit")
                .query("wind_speed_unit", "mph")
                .query("forecast_days", &days.to_string())
                .call()
                .and_then(|r| r.into_json().map_err(ureq::Error::from))
            {
                Ok(v) => v,
                // open-meteo's main API has outages; for US locations the National Weather
                // Service is a rock-solid keyless fallback (7-day horizon, no sunset).
                Err(_) => return nws_daily(lat, lon),
            };
            let d = &w["daily"];
            let dates = d["time"].as_array().cloned().unwrap_or_default();
            let mut out = Vec::new();
            for (i, date) in dates.iter().enumerate() {
                let Some(date) = date.as_str() else { continue };
                let weekday = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                    .map(|nd| nd.format("%a").to_string())
                    .unwrap_or_default();
                out.push(DayForecast {
                    date: date.to_string(),
                    weekday,
                    desc: wmo(d["weather_code"][i].as_i64().unwrap_or(-1)).to_string(),
                    hi_f: d["temperature_2m_max"][i].as_f64().unwrap_or(0.0),
                    lo_f: d["temperature_2m_min"][i].as_f64().unwrap_or(0.0),
                    precip_prob: d["precipitation_probability_max"][i].as_f64().unwrap_or(0.0),
                    wind_mph: d["wind_speed_10m_max"][i].as_f64().unwrap_or(0.0),
                    sunset: d["sunset"][i]
                        .as_str()
                        .and_then(|s| s.split('T').nth(1))
                        .unwrap_or("")
                        .to_string(),
                });
            }
            Ok(out)
        })
        .await?
    }
}

/// US National Weather Service fallback: points → gridpoint forecast → daytime periods.
/// Keyless; requires a User-Agent with contact info per NWS policy.
fn nws_daily(lat: f64, lon: f64) -> anyhow::Result<Vec<DayForecast>> {
    const UA: &str = "yantrik-mind (contact: developer@pranab.co.in)";
    let pts: serde_json::Value = ureq::get(&format!("https://api.weather.gov/points/{lat:.4},{lon:.4}"))
        .set("User-Agent", UA)
        .timeout(std::time::Duration::from_secs(15))
        .call()?
        .into_json()?;
    let url = pts["properties"]["forecast"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("NWS: no forecast URL (non-US location?)"))?
        .to_string();
    let fc: serde_json::Value = ureq::get(&url)
        .set("User-Agent", UA)
        .timeout(std::time::Duration::from_secs(15))
        .call()?
        .into_json()?;
    let periods = fc["properties"]["periods"].as_array().cloned().unwrap_or_default();
    // Night temps become the day's low.
    let mut lows: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for p in &periods {
        if !p["isDaytime"].as_bool().unwrap_or(true) {
            if let (Some(d), Some(t)) = (p["startTime"].as_str(), p["temperature"].as_f64()) {
                lows.insert(d[..10].to_string(), t);
            }
        }
    }
    let mut out = Vec::new();
    for p in &periods {
        if !p["isDaytime"].as_bool().unwrap_or(false) {
            continue;
        }
        let Some(date) = p["startTime"].as_str().map(|t| t[..10].to_string()) else { continue };
        let weekday = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
            .map(|nd| nd.format("%a").to_string())
            .unwrap_or_default();
        let wind_mph = p["windSpeed"]
            .as_str()
            .unwrap_or("")
            .split_whitespace()
            .filter_map(|t| t.parse::<f64>().ok())
            .fold(0.0f64, f64::max);
        out.push(DayForecast {
            weekday,
            desc: p["shortForecast"].as_str().unwrap_or("—").to_string(),
            hi_f: p["temperature"].as_f64().unwrap_or(0.0),
            lo_f: lows.get(&date).copied().unwrap_or(0.0),
            precip_prob: p["probabilityOfPrecipitation"]["value"].as_f64().unwrap_or(0.0),
            wind_mph,
            sunset: String::new(),
            date,
        });
    }
    Ok(out)
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

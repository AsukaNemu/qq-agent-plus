// Vision requests share one process-wide round-robin cursor. Status/capability
// checks only describe the pool; allocation happens when an API request starts.
const cursors = new Map();

export function visionModelsOf(setting) {
  const candidates = [setting?.model, ...(Array.isArray(setting?.models) ? setting.models : [])];
  return [...new Set(candidates
    .filter((model) => typeof model === 'string')
    .map((model) => model.trim())
    .filter(Boolean))];
}

export function activeVisionModels(api, now = Date.now()) {
  const models = visionModelsOf({ model: api.model, models: api.visionModels });
  if (api.visionBalanceFrom) {
    const startsAt = Date.parse(api.visionBalanceFrom);
    // A malformed schedule must not activate additional, potentially exhausted
    // endpoints. The configured primary remains usable until the date is fixed.
    if (!Number.isFinite(startsAt) || now < startsAt) return models.slice(0, 1);
  }
  return models;
}

export function selectVisionModel(api) {
  if (!Array.isArray(api?.visionModels)) return api;
  const models = activeVisionModels(api);
  if (models.length < 2) return api;
  const key = JSON.stringify([api.baseUrl, models]);
  const cursor = cursors.get(key) || 0;
  cursors.set(key, (cursor + 1) % models.length);
  return { ...api, model: models[cursor] };
}

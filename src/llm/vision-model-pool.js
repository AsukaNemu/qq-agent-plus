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

export function selectVisionModel(api, availability = () => ({ pending: 0, waitMs: 0 })) {
  if (!Array.isArray(api?.visionModels)) return api;
  const models = activeVisionModels(api);
  if (models.length < 2) return api;
  const key = JSON.stringify([api.baseUrl, models]);
  const cursor = cursors.get(key) || 0;
  let selected = null;
  for (let offset = 0; offset < models.length; offset++) {
    const index = (cursor + offset) % models.length;
    const candidate = { ...api, model: models[index] };
    const state = availability(candidate);
    const choice = { index, api: candidate, ...state };
    if (!choice.pending && !choice.waitMs) {
      selected = choice;
      break;
    }
    // If every route is occupied, wait on the earliest cooldown and shortest
    // queue. Ties keep round-robin order rather than favouring the primary.
    if (!selected || choice.waitMs < selected.waitMs
      || (choice.waitMs === selected.waitMs && choice.pending < selected.pending)) selected = choice;
  }
  cursors.set(key, (selected.index + 1) % models.length);
  return selected.api;
}

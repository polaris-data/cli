import type { Config, PolarisClient } from '@polaris/core';

import type { AccountOutput } from '../schemas.js';

export async function runAccountCommand(
  config: Config,
  client: PolarisClient,
): Promise<{ human: string; json: AccountOutput }> {
  const authSource =
    config.apiKeySource === 'environment'
      ? 'configured via POLARIS_API_KEY'
      : config.apiKeySource === 'credential_store'
        ? 'configured via stored credential'
        : 'not configured';

  if (!config.apiKey) {
    return {
      human: [
        'Polaris account',
        `Base URL: ${config.baseUrl}`,
        `Auth: ${authSource}`,
        'Status: not signed in',
        'Run `polaris login` to sign in.',
      ].join('\n'),
      json: {
        base_url: config.baseUrl,
        auth: authSource,
        status: 'not signed in',
        user_id: null,
        email: null,
        plan: null,
        provider: null,
        key_id: null,
      },
    };
  }

  const account = await client.fetchAccount();
  const displayName = account.identity.display_name ?? account.identity.email ?? account.user_id;
  const lines = [
    'Polaris account',
    `Base URL: ${config.baseUrl}`,
    `Auth: ${authSource}`,
    `Status: signed in as ${displayName}`,
    `User ID: ${account.user_id}`,
  ];
  if (account.identity.email) lines.push(`Email: ${account.identity.email}`);
  lines.push(`Plan: ${account.subscription.tier}`);
  lines.push(`Provider: ${account.auth.provider}`);
  if (account.auth.key_id) lines.push(`Key ID: ${account.auth.key_id}`);

  return {
    human: lines.join('\n'),
    json: {
      base_url: config.baseUrl,
      auth: authSource,
      status: `signed in as ${displayName}`,
      user_id: account.user_id,
      email: account.identity.email ?? null,
      plan: account.subscription.tier,
      provider: account.auth.provider,
      key_id: account.auth.key_id ?? null,
    },
  };
}

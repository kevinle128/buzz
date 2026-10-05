import 'package:nostr/nostr.dart' as nostr;

import 'nostr_models.dart';

final _relayPubkeyPattern = RegExp(r'^[0-9a-f]{64}$');

/// Verified kind 39008 App metadata head.
final class AppMetadata {
  final String appId;
  final String name;
  final String? description;
  final String? picture;
  final String status;
  final String eventId;
  final String relayPubkey;
  final int updatedAt;

  const AppMetadata({
    required this.appId,
    required this.name,
    required this.status,
    required this.eventId,
    required this.relayPubkey,
    required this.updatedAt,
    this.description,
    this.picture,
  });
}

/// Visible App actor after fail-closed verification.
class AppActor {
  final String appId;
  final String name;
  final String? picture;
  final String signerPubkey;

  const AppActor({
    required this.appId,
    required this.name,
    required this.signerPubkey,
    this.picture,
  });
}

String? _normalizeRelayPubkey(String? pubkey) {
  if (pubkey == null) return null;
  final normalized = pubkey.trim().toLowerCase();
  return _relayPubkeyPattern.hasMatch(normalized) ? normalized : null;
}

/// Canonical lowercase hyphenated UUID, or null when malformed.
String? parseCanonicalAppId(String? value) {
  if (value == null || value.length != 36) return null;
  for (var index = 0; index < value.length; index += 1) {
    final code = value.codeUnitAt(index);
    if (index == 8 || index == 13 || index == 18 || index == 23) {
      if (code != 45) return null;
      continue;
    }
    final isDigit = code >= 48 && code <= 57;
    final isLowerHex = code >= 97 && code <= 102;
    if (!isDigit && !isLowerHex) return null;
  }
  return value;
}

({bool ok, String? value}) _uniqueTag(List<List<String>> tags, String name) {
  String? found;
  for (final tag in tags) {
    if (tag.isEmpty || tag[0] != name) continue;
    if (tag.length != 2) return (ok: false, value: null);
    if (found != null) return (ok: false, value: null);
    found = tag[1];
  }
  return (ok: true, value: found);
}

bool _hasValidEventIdAndSignature(NostrEvent event) {
  try {
    nostr.Event(
      event.id,
      event.pubkey,
      event.createdAt,
      event.kind,
      event.tags,
      event.content,
      event.sig,
      verify: true,
    );
    return true;
  } catch (_) {
    return false;
  }
}

/// Parse one kind 39008 event. Returns null on any validation failure.
AppMetadata? parseAppMetadata(NostrEvent event, String? relaySelfPubkey) {
  final relayPubkey = _normalizeRelayPubkey(relaySelfPubkey);
  if (relayPubkey == null || event.kind != EventKind.appMetadata) {
    return null;
  }
  if (_normalizeRelayPubkey(event.pubkey) != relayPubkey) {
    return null;
  }
  if (!_hasValidEventIdAndSignature(event)) {
    return null;
  }

  final dTag = _uniqueTag(event.tags, 'd');
  final nameTag = _uniqueTag(event.tags, 'name');
  final statusTag = _uniqueTag(event.tags, 'status');
  final pictureTag = _uniqueTag(event.tags, 'picture');
  if (!dTag.ok || !nameTag.ok || !statusTag.ok || !pictureTag.ok) {
    return null;
  }

  final appId = parseCanonicalAppId(dTag.value);
  final name = nameTag.value;
  final status = statusTag.value;
  if (appId == null ||
      name == null ||
      name.isEmpty ||
      (status != 'active' && status != 'disabled')) {
    return null;
  }

  return AppMetadata(
    appId: appId,
    name: name,
    description: event.content.isEmpty ? null : event.content,
    picture: (pictureTag.value == null || pictureTag.value!.isEmpty)
        ? null
        : pictureTag.value,
    status: status!,
    eventId: event.id,
    relayPubkey: relayPubkey,
    updatedAt: event.createdAt,
  );
}

bool _isNewerHead(NostrEvent candidate, NostrEvent current) {
  if (candidate.createdAt != current.createdAt) {
    return candidate.createdAt > current.createdAt;
  }
  return candidate.id.compareTo(current.id) < 0;
}

/// Fold the latest valid metadata head per App UUID.
Map<String, AppMetadata> foldAppMetadataHeads(
  Iterable<NostrEvent> events,
  String? relaySelfPubkey,
) {
  final heads = <String, ({NostrEvent event, AppMetadata metadata})>{};
  for (final event in events) {
    final metadata = parseAppMetadata(event, relaySelfPubkey);
    if (metadata == null) continue;
    final current = heads[metadata.appId];
    if (current != null && !_isNewerHead(event, current.event)) {
      continue;
    }
    heads[metadata.appId] = (event: event, metadata: metadata);
  }
  return {for (final entry in heads.entries) entry.key: entry.value.metadata};
}

String? _uniqueAppId(List<List<String>> tags) {
  String? found;
  for (final tag in tags) {
    if (tag.isEmpty || tag[0] != 'buzz:app') continue;
    if (tag.length != 2) return null;
    if (found != null) return null;
    found = tag[1];
  }
  return parseCanonicalAppId(found);
}

/// Resolve a kind 9 App actor, or null to keep the event signer.
AppActor? resolveAppActor({
  required NostrEvent event,
  required Map<String, AppMetadata> apps,
  String? relaySelfPubkey,
}) {
  final relayPubkey = _normalizeRelayPubkey(relaySelfPubkey);
  final appId = _uniqueAppId(event.tags);
  final metadata = appId == null ? null : apps[appId];
  if (event.kind == EventKind.streamMessage &&
      relayPubkey != null &&
      _normalizeRelayPubkey(event.pubkey) == relayPubkey &&
      _hasValidEventIdAndSignature(event) &&
      appId != null &&
      metadata != null &&
      metadata.appId == appId &&
      _normalizeRelayPubkey(metadata.relayPubkey) == relayPubkey) {
    return AppActor(
      appId: metadata.appId,
      name: metadata.name,
      picture: metadata.picture,
      signerPubkey: relayPubkey,
    );
  }
  return null;
}

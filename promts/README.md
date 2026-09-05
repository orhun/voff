# AgentMux — Multi-Agent Terminal Workspace

AgentMux, farklı yapay zekâ sağlayıcılarını seçilebilir yazılım ekibi rolleri altında birleştiren, Ratatui tabanlı ve açık kaynak bir terminal çalışma alanı için hazırlanmış proje/prompt paketidir.

Bu depo henüz uygulama kodu değil; uygulamayı tutarlı biçimde geliştirmek için ürün sınırlarını, mimari kararları, ajan davranışlarını, görev protokolünü ve güvenlik kurallarını tanımlayan başlangıç sözleşmesidir.

## Belge haritası

| Dosya | Amaç |
| --- | --- |
| `DESCRIPTION.md` | Ürünün sade ve kapsamlı anlatımı |
| `AGENTS.md` | Depoda çalışan tüm kodlama ajanlarının ortak kuralları |
| `CODEX.md` | Codex için yürütme ve kalite talimatları |
| `docs/PRODUCT_REQUIREMENTS.md` | Fonksiyonel gereksinimler, kullanıcı hikâyeleri ve kabul ölçütleri |
| `docs/ARCHITECTURE.md` | Rust modülleri, veri akışı, kalıcılık ve teknik kararlar |
| `docs/AGENT_PROTOCOL.md` | Görev atama, mesajlaşma, durum ve devir protokolü |
| `docs/CONFIGURATION.md` | Sağlayıcı, ajan, takım ve izin yapılandırmaları |
| `docs/SECURITY.md` | Komut, dosya, sır ve eklenti güvenliği |
| `docs/ROADMAP.md` | MVP'den kararlı sürüme geliştirme aşamaları |
| `CONTRIBUTING.md` | Açık kaynak katkı süreci |

## Başlangıç promptu

Codex veya başka bir kodlama ajanına şu talimat verilebilir:

> Bu depodaki `AGENTS.md` ve sana özel talimat dosyasını önce eksiksiz oku. Ardından `DESCRIPTION.md`, `docs/PRODUCT_REQUIREMENTS.md`, `docs/ARCHITECTURE.md`, `docs/AGENT_PROTOCOL.md`, `docs/CONFIGURATION.md` ve `docs/SECURITY.md` dosyalarını incele. `docs/ROADMAP.md` içindeki yalnızca ilk tamamlanmamış aşamayı uygula. Belirsizlikte varsayımı belgeleyip en küçük güvenli çözümü seç. Her değişiklikte testleri çalıştır, kabul ölçütlerini doğrula ve kapsam dışı özellik ekleme.

## Temel ilkeler

- Terminal-first ve keyboard-first deneyim
- Sağlayıcıdan bağımsız ajan modeli
- İnsan denetimi ve varsayılan güvenli izinler
- Tek ajan, uzman ekip veya özel formasyon desteği
- İzlenebilir görevler, mesajlar ve araç çağrıları
- Yerel öncelikli yapı; sırların depoya veya günlüklere yazılmaması
- Küçük, test edilebilir Rust crate'leri ve kararlı sınırlar

## Önerilen lisans

İlk sürüm için MIT veya Apache-2.0 önerilir. Lisans seçimi proje sahibi tarafından kesinleştirilmeden `LICENSE` dosyası eklenmemelidir.

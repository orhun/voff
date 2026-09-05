# Ürün Açıklaması

## Vizyon

AgentMux; Codex, Claude, Gemini ve gelecekte eklenecek yerel ya da uzak modelleri tek bir terminal arayüzünde, rol bazlı bir yapay zekâ yazılım ekibi olarak çalıştıran açık kaynak bir orkestrasyon sistemidir.

Kullanıcı bir takım formasyonu kurabilir: örneğin Team Lead olarak Codex, Backend Developer olarak Claude, Frontend Developer olarak Gemini ve Reviewer olarak yerel bir model seçebilir. Roller sağlayıcılara sabit değildir; aynı rol başka bir modelle değiştirilebilir, bütün roller tek ajana verilebilir veya tamamen özel roller oluşturulabilir.

## Problem

Birden fazla yapay zekâ aracıyla yazılım geliştirirken görevler, bağlam, dosya değişiklikleri, izinler ve çıktılar farklı uygulamalara dağılır. Ajanların birbirine kontrollü şekilde iş devretmesi, sonuçları birleştirmesi ve kullanıcının tüm süreci tek yerden izlemesi zordur.

AgentMux bu parçaları bir çatı altında toplar:

- Ajan ve model seçimi
- Ajanlara özel sistem promptları, roller ve araç izinleri
- Üst görevlerin alt görevlere bölünmesi
- Bir ajanın başka bir ajana görev önerebilmesi veya atayabilmesi
- Paralel ve sıralı iş yürütme
- Ortak proje çalışma alanı
- Çakışma, hata, bütçe ve onay yönetimi
- Terminal üzerinden canlı durum, günlük ve diff inceleme

## Hedef kullanıcılar

- Birden fazla AI kodlama aracını birlikte kullanan geliştiriciler
- Tek geliştirici olarak sanal yazılım ekibi kurmak isteyenler
- Sağlayıcı bağımlılığı olmadan ajan iş akışları deneyen açık kaynak toplulukları
- Tekrarlanabilir, denetlenebilir AI destekli geliştirme süreçleri isteyen ekipler

## Temel deneyim

1. Kullanıcı bir çalışma alanı açar.
2. Hazır formasyon seçer veya ajanlarını tek tek tanımlar.
3. Her ajan için sağlayıcı/model, rol promptu, araçlar, bütçe ve izinler belirler.
4. Ana hedefi Team Lead'e veya doğrudan seçtiği ajana verir.
5. Ajan hedefi alt görevlere ayırır; yetkisi varsa görevleri diğer ajanlara devreder.
6. Kullanıcı görev ağacını, canlı çıktıları, maliyeti ve dosya değişikliklerini TUI üzerinden izler.
7. Riskli komutlar ve çakışan değişiklikler insan onayına gelir.
8. Sonuçlar test, review ve kabul ölçütleriyle kapatılır.

## Ürün sınırları

AgentMux bir model eğitme platformu, genel amaçlı masaüstü IDE veya bulut CI hizmeti değildir. MVP; terminal arayüzü, yerel çalışma alanı, görev orkestrasyonu ve sınırlı sayıda sağlayıcı adaptörüne odaklanır. Uzak worker, web paneli, marketplace ve dağıtık takım çalışması sonraki sürümlerdedir.

## Başarı ölçütleri

- Yeni bir kullanıcı 10 dakika içinde ilk takımını kurup görev çalıştırabilmeli.
- Sağlayıcı değiştirmek iş akışının çekirdeğinde kod değişikliği gerektirmemeli.
- Her görev için sahip, durum, girdiler, çıktılar ve araç çağrıları izlenebilmeli.
- Ajanlar izinsiz riskli komut çalıştıramamalı.
- Başarısız veya yarıda kalan oturum yeniden açıldığında sürdürülebilmeli.
- Tek ajan modu, çok ajan mekanizmasının zorunlu karmaşıklığını kullanıcıya yansıtmamalı.

## Çalışma adı

`AgentMux` bir çalışma adıdır. Yayın öncesinde paket adı, alan adı, crates.io ve GitHub uygunluğu ayrıca doğrulanmalıdır.
